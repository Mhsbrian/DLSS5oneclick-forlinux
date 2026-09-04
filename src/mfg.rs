//! Optional RTX 40 DLSS Multi-Frame-Generation unlock (dashdogy/RTX40MFG-Unlock, MIT).
//!
//! A *different* feature from the neural-rendering stack this tool installs:
//! it raises the DLSS Frame-Generation multiplier (2X–6X) on RTX 40 cards for
//! games that already ship Streamline DLSS Frame Generation. It rides in ReShade
//! like the RenoDX HDR mod, so it slots in as an optional per-game toggle.
//!
//! Three files go beside the game exe — `RTX40MFGCore.dll`, `RTX40MFG.asi`,
//! `RTX40MFG-UI.addon64` — plus Ultimate ASI Loader (ThirteenAG) under a proxy
//! DLL the game imports (never `dxgi.dll`, which ReShade owns) and the loader's
//! `[GlobalSets]` in `<proxy>.ini`. Under Proton the proxy needs its own
//! `WINEDLLOVERRIDE`, which `platform::launch_options` reads from the manifest.
//!
//! Windows-only per its author and experimental under Proton: the mod validates
//! a Streamline FG wrapper/provider layout the NVIDIA App supplies on Windows,
//! which Proton's dxvk-nvapi may not match, in which case it "fails closed".
//! The tool installs it correctly and labels it honestly; `diagnose` reads the
//! mod's own log to say whether it actually engaged.

use crate::game::{self, GameStatus};
use crate::net::{self, Progress};
use crate::reshade_ini::Ini;
use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use std::path::Path;

pub const MFG_REPO: &str = "dashdogy/RTX40MFG-Unlock";
pub const UAL_REPO: &str = "ThirteenAG/Ultimate-ASI-Loader";
pub const UAL_ASSET: &str = "Ultimate-ASI-Loader_x64.zip";

/// The three files the mod ships (besides `global.ini`), placed beside the exe.
pub const MFG_CORE: &str = "RTX40MFGCore.dll";
pub const MFG_ASI: &str = "RTX40MFG.asi";
pub const MFG_UI: &str = "RTX40MFG-UI.addon64";
pub const MFG_FILES: [&str; 3] = [MFG_CORE, MFG_ASI, MFG_UI];
pub const MFG_MANIFEST: &str = ".dlss5oneclick-mfg-manifest";

/// Ultimate ASI Loader proxy names, in preference order. Never `dxgi` (ReShade)
/// or `d3d1x` (the render path); these are early-loading, side-channel imports
/// common to games and safe to proxy.
const PROXY_CANDIDATES: [&str; 4] = ["version", "winmm", "dinput8", "wininet"];

/// Whether the game already ships Streamline DLSS Frame Generation — the thing
/// MFG multiplies. Mirrors `game::game_ships_dlss`'s 4-deep walk, looking for
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

/// The first supported ASI-loader proxy the exe imports, or `None` when it
/// imports no candidate (the checkbox is then disabled with that reason).
pub fn pick_proxy(exe: &Path) -> Option<&'static str> {
    let imports = game::pe_imports(exe);
    PROXY_CANDIDATES
        .into_iter()
        .find(|p| imports.iter().any(|i| i == &format!("{p}.dll")))
}

/// Why MFG can or cannot be offered for this game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eligibility {
    /// Offer it; carries the proxy the loader will use.
    Ready(&'static str),
    NotRtx40,
    NoFrameGen,
    NoProxy,
}

impl Eligibility {
    /// A short reason for a disabled checkbox tooltip.
    pub fn reason(&self) -> &'static str {
        match self {
            Eligibility::Ready(_) => "",
            Eligibility::NotRtx40 => {
                "DLSS Multi-Frame-Generation unlock is for RTX 40 cards (RTX 50 has it natively; \
                 RTX 20/30 cannot Frame-Generate)."
            }
            Eligibility::NoFrameGen => {
                "This game has no DLSS Frame Generation to multiply (no Streamline sl.dlss_g.dll)."
            }
            Eligibility::NoProxy => {
                "This game imports no ASI-loader proxy DLL (version/winmm/dinput8/wininet), so the \
                 loader has nothing to attach to."
            }
        }
    }
}

/// Gate the MFG toggle: RTX 40 GPU, the game has Streamline FG, and the exe
/// imports a usable ASI-loader proxy.
pub fn eligible(st: &GameStatus) -> Eligibility {
    let is_rtx40 = matches!(st.gpu.as_ref().map(|(_, t)| *t), Some(crate::gpu::Tier::Rtx40));
    if !is_rtx40 {
        return Eligibility::NotRtx40;
    }
    if !st.has_fg {
        return Eligibility::NoFrameGen;
    }
    match pick_proxy(&st.exe) {
        Some(p) => Eligibility::Ready(p),
        None => Eligibility::NoProxy,
    }
}

/// The ASI-loader proxy this tool recorded for an installed MFG, from the
/// manifest header (`# proxy version`). Lets launch-options and uninstall know
/// which `<proxy>.dll`/`<proxy>.ini` were placed.
pub fn manifest_proxy(game_dir: &Path) -> Option<String> {
    let m = std::fs::read_to_string(game::join_ci(game_dir, &[MFG_MANIFEST])).ok()?;
    m.lines()
        .find_map(|l| l.strip_prefix("# proxy "))
        .map(|s| s.trim().to_owned())
}

fn asset_url(client: &Client, repo: &str, name_re: &str) -> Result<String> {
    let tag = net::latest_tag(client, repo)?;
    net::github_asset_url_html(client, repo, &tag, name_re)
}

/// Download the MFG release + Ultimate ASI Loader, place everything beside the
/// exe, wire the loader, and record a manifest. Idempotent enough to re-run:
/// files are overwritten, the manifest rewritten.
pub fn install(client: &Client, exe: &Path, proxy: &str, progress: Progress) -> Result<Vec<String>> {
    let d = exe.parent().context("exe has no parent")?;
    let work = tempfile::Builder::new().prefix("dlss5o-mfg-").tempdir()?;

    progress(0, "Looking up latest RTX40MFG-Unlock");
    let mfg_url = asset_url(client, MFG_REPO, r#"[^"]+\.zip"#)?;
    let mfg_zip = work.path().join("mfg.zip");
    net::download(client, &mfg_url, &mfg_zip, "RTX40MFG-Unlock", progress)?;

    let mut placed: Vec<String> = Vec::new();
    for f in MFG_FILES {
        install_from_zip(&mfg_zip, f, &game::join_ci(d, &[f]))?;
        placed.push(f.to_owned());
    }

    progress(0, "Looking up latest Ultimate ASI Loader");
    let ual_url = asset_url(client, UAL_REPO, &regex::escape(UAL_ASSET))?;
    let ual_zip = work.path().join("ual.zip");
    net::download(client, &ual_url, &ual_zip, "Ultimate ASI Loader", progress)?;
    // The x64 archive holds one loader DLL under some proxy name; place it as
    // the proxy this game imports.
    let proxy_dll = format!("{proxy}.dll");
    install_first_dll_from_zip(&ual_zip, &game::join_ci(d, &[&proxy_dll]))?;
    placed.push(proxy_dll.clone());

    // The loader reads its config from `<proxy>.ini`; merge the mod's
    // `[GlobalSets]` (LoadPlugins / LoadExtraPlugins=RTX40MFG.asi / …) without
    // disturbing any keys a user already set there.
    let ini_name = format!("{proxy}.ini");
    let ini_path = game::join_ci(d, &[&ini_name]);
    let mut ini = Ini::load(&ini_path);
    let global = read_zip_text(&mfg_zip, "global.ini")?;
    for (section, kv) in Ini::parse(&global).sections {
        for (key, val) in kv {
            ini.set(&section, &key, val);
        }
    }
    ini.save(&ini_path)?;
    placed.push(ini_name);

    let mut manifest = format!("# proxy {proxy}\n");
    manifest.push_str(&placed.join("\n"));
    manifest.push('\n');
    std::fs::write(game::join_ci(d, &[MFG_MANIFEST]), manifest)?;
    placed.push(MFG_MANIFEST.to_owned());

    progress(100, "RTX 40 MFG unlock installed");
    Ok(placed)
}

/// Remove everything an MFG manifest lists, then the manifest. No-op when none.
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

// ── zip helpers (thin wrappers over net::) ─────────────────────────

fn install_from_zip(zip_path: &Path, member: &str, dest: &Path) -> Result<()> {
    crate::installer::install_single_from_zip(zip_path, member, dest)
}

/// Extract the first `*.dll` member (Ultimate ASI Loader ships exactly one).
fn install_first_dll_from_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let f = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("Ultimate ASI Loader download is not a valid zip")?;
    let member = zip
        .file_names()
        .find(|n| n.to_ascii_lowercase().ends_with(".dll"))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Ultimate ASI Loader archive has no DLL"))?;
    net::extract_member(&mut zip, &member, dest)
}

fn read_zip_text(zip_path: &Path, member: &str) -> Result<String> {
    use std::io::Read;
    let f = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("RTX40MFG download is not a valid zip")?;
    let mut file = zip
        .by_name(member)
        .with_context(|| format!("{member} not found in the RTX40MFG release"))?;
    let mut s = String::new();
    file.read_to_string(&mut s)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::testutil::make_pe_with_imports;

    #[test]
    fn pick_proxy_prefers_version_and_never_dxgi() {
        let t = tempfile::tempdir().unwrap();
        let a = make_pe_with_imports(
            &t.path().join("a.exe"),
            &["dxgi.dll", "winmm.dll", "version.dll"],
            2_000_000,
        );
        assert_eq!(pick_proxy(&a), Some("version")); // version beats winmm
        let b = make_pe_with_imports(&t.path().join("b.exe"), &["dxgi.dll", "winmm.dll"], 2_000_000);
        assert_eq!(pick_proxy(&b), Some("winmm"));
        // dxgi alone is never a proxy — ReShade owns it.
        let c = make_pe_with_imports(&t.path().join("c.exe"), &["dxgi.dll", "kernel32.dll"], 2_000_000);
        assert_eq!(pick_proxy(&c), None);
    }

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
