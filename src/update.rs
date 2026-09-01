//! Self-update against GitHub releases, without the API.
//!
//! `github.com/<repo>/releases/latest` answers with a 302 to
//! `/releases/tag/vX.Y.Z`; that Location header is the version check. The
//! binary itself comes from `releases/latest/download/dlss5oneclick.exe`.
//! Replacing a running exe on Windows: rename the running file aside (allowed),
//! move the new one into its place, start it, exit; the next start deletes the
//! `.old` file. The user always decides: nothing is downloaded until they say so.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "faisalkindi/DLSS5oneclick";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub version: String,
    pub tag: String,
    pub url: String,
}

pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let mut it = v
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(k)) => c > k,
        _ => false,
    }
}

/// Latest release tag from the redirect, no API call.
pub fn check() -> Result<Option<Available>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("DLSS5oneclick/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp = client
        .get(format!("https://github.com/{REPO}/releases/latest"))
        .send()
        .context("update check: request failed")?;
    let loc = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            anyhow!(
                "update check: no redirect from releases/latest (HTTP {})",
                resp.status()
            )
        })?;
    let tag = loc
        .rsplit('/')
        .next()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("update check: odd redirect {loc}"))?
        .to_string();
    let version = tag.trim_start_matches('v').to_string();
    if !is_newer(&version, CURRENT) {
        return Ok(None);
    }
    Ok(Some(Available {
        url: format!("https://github.com/{REPO}/releases/download/{tag}/dlss5oneclick.exe"),
        version,
        tag,
    }))
}

/// Download the new exe next to the running one and swap them in. Returns the
/// path of the new exe to launch. Never touches the running file until the
/// download is complete and looks like a Windows executable.
pub fn download_and_swap(av: &Available, progress: &(dyn Fn(u8, &str) + Sync)) -> Result<PathBuf> {
    let me = std::env::current_exe().context("cannot locate the running exe")?;
    let dir = me
        .parent()
        .ok_or_else(|| anyhow!("exe has no parent folder"))?;
    let fresh = dir.join(format!("dlss5oneclick-{}.new", av.version));
    let client = crate::net::client()?;
    crate::net::download(
        &client,
        &av.url,
        &fresh,
        &format!("DLSS5oneclick {}", av.version),
        progress,
    )?;
    let meta = std::fs::metadata(&fresh)?;
    let head = std::fs::read(&fresh)?;
    if meta.len() < 1_000_000 || head.get(..2) != Some(b"MZ") {
        let _ = std::fs::remove_file(&fresh);
        bail!("downloaded file is not a valid executable");
    }
    let old = dir.join("dlss5oneclick.exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&me, &old).context("cannot move the running exe aside")?;
    if let Err(e) = std::fs::rename(&fresh, &me) {
        // put things back
        let _ = std::fs::rename(&old, &me);
        return Err(e).context("cannot move the new exe into place");
    }
    progress(100, "Updated; restarting");
    Ok(me)
}

/// Launch `exe` detached and return; the caller exits afterwards.
pub fn relaunch(exe: &Path) -> Result<()> {
    std::process::Command::new(exe)
        .spawn()
        .with_context(|| format!("cannot start {}", exe.display()))?;
    Ok(())
}

/// Delete the previous binary left by a swap, if any. Safe to call every start.
pub fn cleanup_old() {
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let _ = std::fs::remove_file(dir.join("dlss5oneclick.exe.old"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.6.0", "0.5.2"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.5.2", "0.5.2"));
        assert!(!is_newer("0.5.1", "0.5.2"));
        assert!(!is_newer("garbage", "0.5.2"));
        assert_eq!(parse_version("v0.5.2"), Some((0, 5, 2)));
    }
}
