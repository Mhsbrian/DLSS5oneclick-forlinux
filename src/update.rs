//! Self-update against GitHub releases, without the API.
//!
//! `github.com/<repo>/releases/latest` answers with a 302 to
//! `/releases/tag/vX.Y.Z`; that Location header is the version check. The
//! binary comes from `releases/latest/download/<asset>` — `dlss5oneclick.exe`
//! on Windows, `dlss5oneclick-linux-x86_64` on Linux, and the
//! `dlss5oneclick-x86_64.AppImage` when running from an AppImage (then
//! `current_exe` points into the read-only squashfs mount, so the `$APPIMAGE`
//! file itself is what gets replaced). Swap: rename the running file aside
//! (allowed on both OSes), move the new one into its place, start it, exit;
//! the next start deletes the `.old`. The user always decides: nothing is
//! downloaded until they say so.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "Mhsbrian/DLSS5oneclick-forlinux";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[cfg(windows)]
pub const ASSET: &str = "dlss5oneclick.exe";
#[cfg(not(windows))]
pub const ASSET: &str = "dlss5oneclick-linux-x86_64";
pub const APPIMAGE_ASSET: &str = "dlss5oneclick-x86_64.AppImage";

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

/// Set when running from an AppImage: the mounted image is read-only, this is
/// the real file on disk.
fn appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from)
}

fn asset_name() -> &'static str {
    if appimage().is_some() {
        APPIMAGE_ASSET
    } else {
        ASSET
    }
}

/// The file a successful update must replace.
fn self_target() -> Result<PathBuf> {
    self_target_from(appimage())
}

fn self_target_from(appimage: Option<PathBuf>) -> Result<PathBuf> {
    match appimage {
        Some(p) => Ok(p),
        None => std::env::current_exe().context("cannot locate the running exe"),
    }
}

/// `<name>.old` next to the target, whatever the user renamed the binary to.
fn old_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "dlss5oneclick".into());
    target.with_file_name(format!("{name}.old"))
}

/// Enough header to reject HTML error pages and truncated downloads: a real
/// release binary is at least a megabyte of PE ("MZ") or ELF.
fn looks_like_binary(head: &[u8], len: u64) -> bool {
    len >= 1_000_000 && (head.get(..2) == Some(b"MZ") || head.get(..4) == Some(b"\x7fELF"))
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
        url: format!(
            "https://github.com/{REPO}/releases/download/{tag}/{}",
            asset_name()
        ),
        version,
        tag,
    }))
}

/// Download the new binary next to the running one and swap them in. Returns
/// the path to launch. Never touches the running file until the download is
/// complete and looks like a real executable.
pub fn download_and_swap(av: &Available, progress: &(dyn Fn(u8, &str) + Sync)) -> Result<PathBuf> {
    let target = self_target()?;
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("binary has no parent folder"))?;
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
    if !looks_like_binary(&head, meta.len()) {
        let _ = std::fs::remove_file(&fresh);
        bail!("downloaded file is not a valid executable");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))
            .context("cannot mark the new binary executable")?;
    }
    let old = old_path(&target);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&target, &old).context("cannot move the running binary aside")?;
    if let Err(e) = std::fs::rename(&fresh, &target) {
        // put things back
        let _ = std::fs::rename(&old, &target);
        return Err(e).context("cannot move the new binary into place");
    }
    progress(100, "Updated; restarting");
    Ok(target)
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
    if let Ok(target) = self_target() {
        let _ = std::fs::remove_file(old_path(&target));
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

    #[test]
    fn binary_sniff() {
        let mut elf = b"\x7fELF".to_vec();
        elf.extend([0u8; 64]);
        assert!(looks_like_binary(&elf, 2_000_000));
        assert!(looks_like_binary(b"MZ\x90\x00", 2_000_000));
        assert!(!looks_like_binary(&elf, 10_000)); // truncated
        assert!(!looks_like_binary(b"<!DOCTYPE html>", 2_000_000));
    }

    #[test]
    fn old_name_follows_target() {
        assert_eq!(
            old_path(Path::new("/x/dlss5oneclick.exe")),
            Path::new("/x/dlss5oneclick.exe.old")
        );
        assert_eq!(
            old_path(Path::new("/x/dlss5oneclick-linux-x86_64")),
            Path::new("/x/dlss5oneclick-linux-x86_64.old")
        );
    }

    #[test]
    fn appimage_env_redirects_target() {
        let p = PathBuf::from("/home/u/Apps/dlss5oneclick.AppImage");
        assert_eq!(self_target_from(Some(p.clone())).unwrap(), p);
        // Without $APPIMAGE the running exe is the target.
        assert!(self_target_from(None).unwrap().exists());
    }
}
