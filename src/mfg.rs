//! RTX 40 multi-frame generation: what this tool needs to know about a game for
//! it, and what is left of the first unlock this fork shipped.
//!
//! The unlock itself is upstream's since 0.17 -- mavismmg's add-on on the
//! ReShade route and the pre-SR OptiScaler build's own `AdaMfgUnlock` (see
//! `installer::step_mfg`). Before that this fork placed dashdogy's
//! RTX40MFG-Unlock: three files plus Ultimate ASI Loader under a proxy DLL the
//! game imports, recorded in a manifest. Nothing installs it any more, but games
//! that have it must still come out cleanly -- on Remove, and before the new
//! unlock goes in, since the two patch the same thing in memory and must never
//! run together -- and the launch options keep its proxy's override for as long
//! as it is there.

use crate::game;
use anyhow::Result;
use std::path::Path;

/// The manifest the dashdogy unlock was recorded in: a `# proxy <name>` header,
/// then every file it placed.
pub const MFG_MANIFEST: &str = ".dlss5oneclick-mfg-manifest";

/// Whether the game ships DLSS Frame Generation of its own — the thing any MFG
/// unlock multiplies. Mirrors `game::game_ships_dlss`'s 4-deep walk, looking for
/// `sl.dlss_g.dll` (the Streamline FG plugin) or `nvngx_dlssg.dll` (its model).
pub fn has_streamline_fg(game_dir: &Path) -> bool {
    fn walk(d: &Path, depth: u8) -> bool {
        let Ok(rd) = std::fs::read_dir(d) else {
            return false;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                let n = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if n == "sl.dlss_g.dll" || n == "nvngx_dlssg.dll" {
                    return true;
                }
            } else if depth > 0 && p.is_dir() && walk(&p, depth - 1) {
                return true;
            }
        }
        false
    }
    walk(game_dir, 4)
}

/// The ASI-loader proxy recorded for an installed dashdogy unlock, from the
/// manifest header (`# proxy version`), so the launch options keep its
/// `<proxy>=n,b` override while it is there.
pub fn manifest_proxy(game_dir: &Path) -> Option<String> {
    let m = std::fs::read_to_string(game::join_ci(game_dir, &[MFG_MANIFEST])).ok()?;
    m.lines()
        .find_map(|l| l.strip_prefix("# proxy "))
        .map(|s| s.trim().to_owned())
}

/// Remove everything a dashdogy manifest lists, then the manifest. No-op when
/// there is none.
pub fn uninstall(game_dir: &Path, removed: &mut Vec<String>) -> Result<()> {
    let manifest = game::join_ci(game_dir, &[MFG_MANIFEST]);
    let Ok(list) = std::fs::read_to_string(&manifest) else {
        return Ok(());
    };
    for name in list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let p = game::join_ci(game_dir, &[name]);
        if p.is_file() {
            std::fs::remove_file(&p)?;
            removed.push(name.to_owned());
        }
    }
    if manifest.is_file() {
        std::fs::remove_file(&manifest)?;
        removed.push(MFG_MANIFEST.to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_streamline_frame_generation() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert!(!has_streamline_fg(d));
        std::fs::create_dir_all(d.join("bin/x64")).unwrap();
        std::fs::write(d.join("bin/x64/sl.dlss_g.dll"), b"x").unwrap();
        assert!(has_streamline_fg(d));
    }

    #[test]
    fn manifest_proxy_round_trip_and_uninstall() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        // Simulate an install's placed files + manifest.
        for f in ["RTX40MFGCore.dll", "RTX40MFG.asi", "version.dll", "version.ini"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        std::fs::write(
            d.join(MFG_MANIFEST),
            "# proxy version\nRTX40MFGCore.dll\nRTX40MFG.asi\nversion.dll\nversion.ini\n",
        )
        .unwrap();
        assert_eq!(manifest_proxy(d).as_deref(), Some("version"));
        let mut removed = Vec::new();
        uninstall(d, &mut removed).unwrap();
        assert!(removed.contains(&"version.dll".to_string()));
        assert!(!d.join("version.dll").is_file());
        assert!(!d.join(MFG_MANIFEST).is_file());
    }
}
