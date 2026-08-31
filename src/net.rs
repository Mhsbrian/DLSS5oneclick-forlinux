//! HTTP helpers: GitHub JSON, streamed downloads with progress, zip member extraction.

use anyhow::{bail, Context, Result};
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

pub fn get_json(client: &Client, url: &str) -> Result<serde_json::Value> {
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .with_context(|| format!("request failed: {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    resp.json().with_context(|| format!("bad JSON from {url}"))
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

/// Stream `url` to `dest` (via a `.part` file), reporting progress.
pub fn download(
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
