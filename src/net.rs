//! HTTP helpers: GitHub JSON, streamed downloads with progress, zip member extraction.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::Duration;

pub type Progress<'a> = &'a (dyn Fn(u8, &str) + Sync);

pub fn client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("DLSS5oneclick/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()
        .context("cannot build HTTP client")
}

/// GitHub API calls are capped at 60/hour per IP without a token. Honour
/// `GITHUB_TOKEN` when a user sets one; otherwise callers fall back to the
/// HTML release pages, which have no such cap.
pub fn get_json_github(client: &Client, url: &str) -> Result<serde_json::Value> {
    let mut req = client
        .get(url)
        .header("Accept", "application/vnd.github+json");
    if let Ok(tok) = std::env::var("GITHUB_TOKEN") {
        if !tok.trim().is_empty() {
            req = req.bearer_auth(tok.trim());
        }
    }
    if std::env::var("DLSS5ONECLICK_NO_API").is_ok() {
        bail!("API disabled by DLSS5ONECLICK_NO_API");
    }
    let resp = req
        .send()
        .with_context(|| format!("request failed: {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    resp.json().with_context(|| format!("bad JSON from {url}"))
}

/// Release tags of `owner/repo` starting with `prefix`, read from the HTML
/// releases pages (newest first, up to `pages` pages of 10). No API, no cap.
pub fn github_release_tags_html(
    client: &Client,
    repo: &str,
    prefix: &str,
    pages: usize,
) -> Result<Vec<String>> {
    let re = regex::Regex::new(&format!(
        r#"/{}/releases/tag/({}[A-Za-z0-9._-]*)"#,
        regex::escape(repo),
        regex::escape(prefix)
    ))
    .unwrap();
    let mut tags: Vec<String> = Vec::new();
    for page in 1..=pages {
        let html = get_text(
            client,
            &format!("https://github.com/{repo}/releases?page={page}"),
        )?;
        let mut found_any = false;
        for c in re.captures_iter(&html) {
            let t = c[1].to_string();
            if !tags.contains(&t) {
                tags.push(t);
            }
            found_any = true;
        }
        // Stop once a page had matches and the next one would be older releases,
        // or when the page lists nothing at all.
        if found_any || !html.contains("/releases/tag/") {
            if found_any && page >= 1 {
                // one more page catches prefixes split across the boundary
                if page == pages {
                    break;
                }
                let html2 = get_text(
                    client,
                    &format!("https://github.com/{repo}/releases?page={}", page + 1),
                )?;
                for c in re.captures_iter(&html2) {
                    let t = c[1].to_string();
                    if !tags.contains(&t) {
                        tags.push(t);
                    }
                }
            }
            break;
        }
    }
    Ok(tags)
}

/// Download URL of the first asset of `tag` whose file name matches `name_re`,
/// read from GitHub's expanded-assets HTML fragment. No API.
pub fn github_asset_url_html(
    client: &Client,
    repo: &str,
    tag: &str,
    name_re: &str,
) -> Result<String> {
    let html = get_text(
        client,
        &format!("https://github.com/{repo}/releases/expanded_assets/{tag}"),
    )?;
    let re = regex::Regex::new(&format!(
        r#"/{}/releases/download/{}/({})"#,
        regex::escape(repo),
        regex::escape(tag),
        name_re
    ))
    .unwrap();
    let m = re
        .captures(&html)
        .ok_or_else(|| anyhow!("no asset matching {name_re} in {repo} {tag}"))?;
    Ok(format!(
        "https://github.com/{repo}/releases/download/{tag}/{}",
        &m[1]
    ))
}

pub fn get_text(client: &Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("request failed: {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    resp.text().with_context(|| format!("bad body from {url}"))
}

fn fmt_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", n / 1024)
    }
}

/// Stream `url` to `dest`, retrying connection-level failures (GitHub's CDN edge
/// occasionally answers with a broken TLS record; the next attempt succeeds).
pub fn download(
    client: &Client,
    url: &str,
    dest: &Path,
    label: &str,
    progress: Progress,
) -> Result<()> {
    let mut last = None;
    for attempt in 1..=4u32 {
        match download_once(client, url, dest, label, progress) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let msg = format!("{e:#}");
                let retryable = msg.contains("error sending request")
                    || msg.contains("Connect")
                    || msg.contains("corrupt message")
                    || msg.contains("connection")
                    || msg.contains("timed out")
                    || msg.contains("reset");
                if !retryable || attempt == 4 {
                    return Err(e);
                }
                progress(0, &format!("{label}: retrying ({attempt}/3)"));
                std::thread::sleep(std::time::Duration::from_secs(2 * attempt as u64));
                last = Some(e);
            }
        }
    }
    Err(last.unwrap())
}

/// One attempt: stream to a `.part` file, reporting percent + KB/MB downloaded.
fn download_once(
    client: &Client,
    url: &str,
    dest: &Path,
    label: &str,
    progress: Progress,
) -> Result<()> {
    let mut resp = client
        .get(url)
        .send()
        .with_context(|| format!("download failed: {label}"))?;
    if !resp.status().is_success() {
        bail!("{label}: HTTP {}", resp.status());
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    let total = resp.content_length().unwrap_or(0);
    let mut done: u64 = 0;
    let mut buf = vec![0u8; 1 << 18];
    let result: Result<()> = (|| {
        let mut out = fs::File::create(&part)?;
        loop {
            let n = resp.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            if total > 0 {
                let pct = (done as f64 / total as f64 * 100.0).min(99.0) as u8;
                progress(
                    pct,
                    &format!("{label}: {} / {}", fmt_bytes(done), fmt_bytes(total)),
                );
            } else {
                progress(0, &format!("{label}: {}", fmt_bytes(done)));
            }
        }
        out.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&part);
        return Err(e).with_context(|| format!("download failed: {label}"));
    }
    fs::rename(&part, dest)?;
    Ok(())
}

/// Copy one zip member to an exact destination (the zip's own path is never used).
pub fn extract_member<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    member: &str,
    dest: &Path,
) -> Result<()> {
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let mut f = zip
        .by_name(member)
        .with_context(|| format!("zip member missing: {member}"))?;
    let mut out = fs::File::create(dest)?;
    std::io::copy(&mut f, &mut out)?;
    Ok(())
}

/// File members whose name matches `re`.
pub fn members_matching<R: Read + Seek>(
    zip: &zip::ZipArchive<R>,
    re: &regex::Regex,
) -> Vec<String> {
    zip.file_names()
        .filter(|n| !n.ends_with('/') && re.is_match(n))
        .map(str::to_owned)
        .collect()
}

pub fn file_name(member: &str) -> &str {
    member.rsplit('/').next().unwrap_or(member)
}
