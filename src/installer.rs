//! The five install steps, in the order the DLSS5-Feeder README lists them.
//!
//! Sources (verified 2026-08-31):
//! 1. ReShade add-on build — https://reshade.me links `/downloads/ReShade_Setup_<ver>_Addon.exe`;
//!    that exe has an appended ZIP with ReShade64.dll / ReShade32.dll. Dropped as dxgi.dll.
//! 2. DLSS5-Feeder — jlrouzies-fr/DLSS5-Feeder latest release, loose assets
//!    `dlss5-feed.addon64` + `DLSS5_Feed.fx` (the `feed-vk-layer.zip` is Vulkan-only, unused).
//! 3. LumeniteFX — umar-afzaal/LumeniteFX branch `mainline` (no releases):
//!    Shaders/lumenite_*.fx, Shaders/include/*.fxh, Textures/lumenite_bluenoise256.png.
//! 4. DLSS 5 add-on — RankFTW/rhi-repo releases: `renodx-dlss5-*` (renodx-dlss5.addon64),
//!    `dlssnr-*` (nvngx_dlssnr.dll), `dlss-*` (nvngx_dlss.dll; not dlssg-/dlssd-).
//! 5. ReShade.ini + ReShadePreset.ini: DLSS5_MV_PROVIDER=3, Lumenite_Kernel above DLSS5_Feed.

use crate::game::{self, GameStatus};
use crate::net::{self, Progress};
use crate::reshade_ini;
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const RESHADE_HOME: &str = "https://reshade.me";
pub const FEEDER_LATEST: &str = "https://api.github.com/repos/jlrouzies-fr/DLSS5-Feeder/releases/latest";
pub const LUMENITE_ZIP: &str = "https://codeload.github.com/umar-afzaal/LumeniteFX/zip/refs/heads/mainline";
pub const RHI_RELEASES: &str = "https://api.github.com/repos/RankFTW/rhi-repo/releases?per_page=100";

pub struct Step {
    pub name: &'static str,
    pub run: fn(&Client, &GameStatus, &Path, Progress) -> Result<Vec<String>>,
}

pub const STEPS: [Step; 5] = [
    Step { name: "ReShade (add-on build)", run: step_reshade },
    Step { name: "DLSS5-Feeder", run: step_feeder },
    Step { name: "LumeniteFX motion vectors", run: step_lumenite },
    Step { name: "DLSS 5 add-on + models", run: step_dlss5 },
    Step { name: "ReShade config", run: step_config },
];

// ── release picking ────────────────────────────────────────────────

fn ver_key(tag: &str, prefix: &str) -> Vec<u64> {
    Regex::new(r"\d+").unwrap()
        .find_iter(&tag[prefix.len()..])
        .filter_map(|m| m.as_str().parse().ok())
        .collect()
}

/// Newest rhi-repo release whose tag is `prefix` + digits; returns (tag, first asset URL).
pub fn pick_latest_asset(releases: &[Value], prefix: &str) -> Result<(String, String)> {
    let mut cands: Vec<(Vec<u64>, String, String)> = releases
        .iter()
        .filter_map(|r| {
            let tag = r.get("tag_name")?.as_str()?;
            let rest = tag.strip_prefix(prefix)?;
            if !rest.chars().next()?.is_ascii_digit() {
                return None; // "dlss-" must not match "dlssg-"
            }
            let url = r.get("assets")?.as_array()?.first()?.get("browser_download_url")?.as_str()?;
            Some((ver_key(tag, prefix), tag.to_owned(), url.to_owned()))
        })
        .collect();
    if cands.is_empty() {
        bail!("no release with tag prefix '{prefix}' found");
    }
    cands.sort();
    let (_, tag, url) = cands.pop().unwrap();
    Ok((tag, url))
}

/// Required loose Feeder assets -> URL.
pub fn pick_feeder_assets(release: &Value) -> Result<(String, String)> {
    let mut addon = None;
    let mut fx = None;
    for a in release.get("assets").and_then(Value::as_array).into_iter().flatten() {
        let (Some(name), Some(url)) = (
            a.get("name").and_then(Value::as_str),
            a.get("browser_download_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        if name.eq_ignore_ascii_case(game::FEEDER_ADDON) {
            addon = Some(url.to_owned());
        } else if name.eq_ignore_ascii_case(game::FEEDER_FX) {
            fx = Some(url.to_owned());
        }
    }
    let tag = release.get("tag_name").and_then(Value::as_str).unwrap_or("?");
    match (addon, fx) {
        (Some(a), Some(f)) => Ok((a, f)),
        (a, f) => {
            let mut missing = Vec::new();
            if a.is_none() { missing.push(game::FEEDER_ADDON); }
            if f.is_none() { missing.push(game::FEEDER_FX); }
            bail!("DLSS5-Feeder release {tag} is missing: {}", missing.join(", "))
        }
    }
}

// ── step 1: ReShade ────────────────────────────────────────────────

pub fn resolve_reshade_setup(client: &Client) -> Result<(String, String)> {
    let html = net::get_text(client, RESHADE_HOME)?;
    let re = Regex::new(r"/downloads/ReShade_Setup_([\d.]+)_Addon\.exe").unwrap();
    let m = re.captures(&html).ok_or_else(|| anyhow!("ReShade add-on installer link not found on reshade.me"))?;
    Ok((m[1].to_owned(), format!("{RESHADE_HOME}{}", &m[0])))
}

pub fn install_reshade_from_setup(setup_exe: &Path, game_dir: &Path, bitness: u8) -> Result<Vec<String>> {
    let dll = if bitness == 64 { "ReShade64.dll" } else { "ReShade32.dll" };
    let f = fs::File::open(setup_exe)?;
    let mut zip = zip::ZipArchive::new(f).context("ReShade installer has no readable archive")?;
    net::extract_member(&mut zip, dll, &game_dir.join(game::RESHADE_PROXY))
        .with_context(|| format!("{} does not contain {dll}", setup_exe.display()))?;
    Ok(vec![game::RESHADE_PROXY.into()])
}

fn step_reshade(client: &Client, st: &GameStatus, work: &Path, progress: Progress) -> Result<Vec<String>> {
    if st.reshade {
        progress(100, "ReShade already installed");
        return Ok(vec![]);
    }
    let proxy = st.game_dir().join(game::RESHADE_PROXY);
    if proxy.is_file() {
        bail!("{} exists but is not ReShade (DXVK, Special K, another injector?). Remove it first.", game::RESHADE_PROXY);
    }
    progress(0, "Looking up latest ReShade");
    let (ver, url) = resolve_reshade_setup(client)?;
    let setup = work.join(format!("ReShade_Setup_{ver}_Addon.exe"));
    net::download(client, &url, &setup, "ReShade", progress)?;
    install_reshade_from_setup(&setup, st.game_dir(), st.bitness)
}

// ── step 2: DLSS5-Feeder ───────────────────────────────────────────

fn step_feeder(client: &Client, st: &GameStatus, _work: &Path, progress: Progress) -> Result<Vec<String>> {
    if st.feeder {
        progress(100, "DLSS5-Feeder already installed");
        return Ok(vec![]);
    }
    progress(0, "Looking up latest DLSS5-Feeder");
    let release = net::get_json(client, FEEDER_LATEST)?;
    let (addon_url, fx_url) = pick_feeder_assets(&release)?;
    let d = st.game_dir();
    let shaders = d.join("reshade-shaders").join("Shaders");
    net::download(client, &addon_url, &d.join(game::FEEDER_ADDON), game::FEEDER_ADDON, progress)?;
    net::download(client, &fx_url, &shaders.join(game::FEEDER_FX), game::FEEDER_FX, progress)?;
    Ok(vec![game::FEEDER_ADDON.into(), format!("reshade-shaders/Shaders/{}", game::FEEDER_FX)])
}

// ── step 3: LumeniteFX ─────────────────────────────────────────────

pub fn install_lumenite_from_zip(zip_path: &Path, game_dir: &Path) -> Result<Vec<String>> {
    let shaders = game_dir.join("reshade-shaders").join("Shaders");
    let textures = game_dir.join("reshade-shaders").join("Textures");
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("LumeniteFX download is not a valid zip")?;
    let fx = net::members_matching(&zip, &Regex::new(r"(?i)/Shaders/lumenite_[^/]+\.fx$").unwrap());
    let fxh = net::members_matching(&zip, &Regex::new(r"(?i)/Shaders/include/[^/]+\.fxh$").unwrap());
    let png = net::members_matching(&zip, &Regex::new(r"(?i)/Textures/lumenite_bluenoise256\.png$").unwrap());
    if fx.is_empty() || png.is_empty() {
        bail!("LumeniteFX archive layout changed; shaders or texture not found");
    }
    let mut installed = Vec::new();
    for (members, dir, rel) in [
        (&fx, shaders.clone(), "reshade-shaders/Shaders"),
        (&fxh, shaders.join("include"), "reshade-shaders/Shaders/include"),
        (&png, textures, "reshade-shaders/Textures"),
    ] {
        for m in members {
            let name = net::file_name(m);
            net::extract_member(&mut zip, m, &dir.join(name))?;
            installed.push(format!("{rel}/{name}"));
        }
    }
    Ok(installed)
}

fn step_lumenite(client: &Client, st: &GameStatus, work: &Path, progress: Progress) -> Result<Vec<String>> {
    if st.lumenite {
        progress(100, "LumeniteFX already installed");
        return Ok(vec![]);
    }
    let z = work.join("LumeniteFX.zip");
    net::download(client, LUMENITE_ZIP, &z, "LumeniteFX", progress)?;
    install_lumenite_from_zip(&z, st.game_dir())
}

// ── step 4: DLSS 5 add-on + models ─────────────────────────────────

pub fn install_single_from_zip(zip_path: &Path, member_name: &str, dest: &Path) -> Result<()> {
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).with_context(|| format!("{} is not a valid zip", zip_path.display()))?;
    let hit = zip
        .file_names()
        .find(|n| net::file_name(n).eq_ignore_ascii_case(member_name))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{} does not contain {member_name}", zip_path.display()))?;
    net::extract_member(&mut zip, &hit, dest)
}

fn step_dlss5(client: &Client, st: &GameStatus, work: &Path, progress: Progress) -> Result<Vec<String>> {
    let plan = [
        ("renodx-dlss5-", game::DLSS5_ADDON, st.dlss5_addon),
        ("dlssnr-", game::DLSSNR_DLL, st.dlssnr),
        ("dlss-", game::DLSS_DLL, st.dlss),
    ];
    if plan.iter().all(|(_, _, present)| *present) {
        progress(100, "DLSS 5 add-on already present");
        return Ok(vec![]);
    }
    progress(0, "Looking up DLSS 5 add-on releases");
    let releases = net::get_json(client, RHI_RELEASES)?;
    let releases = releases.as_array().ok_or_else(|| anyhow!("unexpected response from GitHub for rhi-repo"))?;
    let mut installed = Vec::new();
    for (prefix, fname, present) in plan {
        if present {
            continue;
        }
        let (tag, url) = pick_latest_asset(releases, prefix)?;
        let z = work.join(format!("{tag}.zip"));
        net::download(client, &url, &z, fname, progress)?;
        install_single_from_zip(&z, fname, &st.game_dir().join(fname))?;
        installed.push(format!("{fname} ({tag})"));
    }
    Ok(installed)
}

// ── step 5: config ─────────────────────────────────────────────────

fn step_config(_c: &Client, st: &GameStatus, _w: &Path, progress: Progress) -> Result<Vec<String>> {
    reshade_ini::write_reshade_ini(st.game_dir())?;
    reshade_ini::write_preset(st.game_dir())?;
    progress(100, "ReShade.ini + ReShadePreset.ini written");
    Ok(vec![game::RESHADE_INI.into(), game::RESHADE_PRESET.into()])
}

// ── driver ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState { Start, Done, Error }

pub fn run_all(
    exe: &Path,
    progress: Progress,
    step_cb: &(dyn Fn(usize, &str, StepState, &str) + Sync),
) -> Result<Vec<(String, Vec<String>)>> {
    let mut st = game::inspect(exe)?;
    if !st.problems.is_empty() {
        bail!("{}", st.problems.join("\n"));
    }
    let client = net::client()?;
    let work = tempfile::Builder::new().prefix("dlss5oneclick-").tempdir()?;
    let mut results = Vec::new();
    for (i, step) in STEPS.iter().enumerate() {
        step_cb(i, step.name, StepState::Start, "");
        match (step.run)(&client, &st, work.path(), progress) {
            Ok(files) => {
                let detail = if files.is_empty() { "already present".to_owned() } else { files.join(", ") };
                step_cb(i, step.name, StepState::Done, &detail);
                results.push((step.name.to_owned(), files));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                step_cb(i, step.name, StepState::Error, &msg);
                return Err(anyhow!("{}: {msg}", step.name));
            }
        }
        st = game::inspect(exe)?;
    }
    Ok(results)
}

/// Remove everything this tool places except ReShade itself and nvngx_dlss.dll.
pub fn uninstall(exe: &Path) -> Result<Vec<String>> {
    let d = exe.parent().context("exe has no parent")?;
    let shaders = d.join("reshade-shaders").join("Shaders");
    let include = shaders.join("include");
    let mut targets: Vec<PathBuf> = vec![
        d.join(game::FEEDER_ADDON),
        d.join(game::DLSS5_ADDON),
        d.join(game::DLSSNR_DLL),
        shaders.join(game::FEEDER_FX),
        d.join("reshade-shaders").join("Textures").join(game::LUMENITE_BLUENOISE),
    ];
    for (dir, ext) in [(&shaders, "fx"), (&include, "fxh")] {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if name.starts_with("lumenite_") && name.ends_with(&format!(".{ext}")) {
                    targets.push(e.path());
                }
            }
        }
    }
    let mut removed = Vec::new();
    for t in targets {
        if t.is_file() {
            fs::remove_file(&t)?;
            removed.push(t.strip_prefix(d).unwrap_or(&t).to_string_lossy().replace('\\', "/"));
        }
    }
    if include.is_dir() && fs::read_dir(&include)?.next().is_none() {
        fs::remove_dir(&include)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::testutil::*;
    use serde_json::json;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn feeder_release() -> Value {
        let names = ["dlss5-feed-host64.exe", "dlss5-feed.addon32", "dlss5-feed.addon64", "DLSS5_Feed.fx", "feed-vk-layer.zip"];
        json!({"tag_name": "v0.6.0-beta.1", "assets": names.iter().map(|n| json!({"name": n, "browser_download_url": format!("https://x/{n}")})).collect::<Vec<_>>()})
    }

    fn rhi_releases() -> Vec<Value> {
        ["streamline-2.13.0.0", "renodx-dlss5-4.55", "renodx-dlss5-4.5", "renodx-dlss5-3.3.4",
         "dlssnr-310.8.SF-v2", "dlssnr-310.8.SF", "dlssg-310.8.0", "dlssd-310.7.129",
         "dlss-310.8.0", "dlss-310.7.129", "DLSS-Enabler-4.9.0.7"]
            .iter()
            .map(|t| json!({"tag_name": t, "assets": [{"browser_download_url": format!("https://x/{t}.zip")}]}))
            .collect()
    }

    #[test]
    fn feeder_assets_are_loose_files_not_vk_zip() {
        let (a, f) = pick_feeder_assets(&feeder_release()).unwrap();
        assert_eq!(a, "https://x/dlss5-feed.addon64");
        assert_eq!(f, "https://x/DLSS5_Feed.fx");
        let bad = json!({"tag_name": "v9", "assets": [{"name": "feed-vk-layer.zip", "browser_download_url": "u"}]});
        assert!(pick_feeder_assets(&bad).unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn latest_asset_versions_and_prefix_isolation() {
        let r = rhi_releases();
        assert_eq!(pick_latest_asset(&r, "renodx-dlss5-").unwrap().0, "renodx-dlss5-4.55");
        assert_eq!(pick_latest_asset(&r, "dlssnr-").unwrap().0, "dlssnr-310.8.SF-v2");
        assert_eq!(pick_latest_asset(&r, "dlss-").unwrap().0, "dlss-310.8.0");
        assert!(pick_latest_asset(&r, "nothing-").is_err());
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])], prefix: &[u8]) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(prefix).unwrap();
        let mut w = zip::ZipWriter::new(f);
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn reshade_from_setup_exe_with_prepended_stub() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let setup = t.path().join("ReShade_Setup_6.8.0_Addon.exe");
        let mut dll = b"MZ".to_vec();
        dll.extend(std::iter::repeat_n(0u8, 1 << 20));
        dll.extend_from_slice(b"ReShade");
        write_zip(&setup, &[("ReShade64.dll", &dll), ("ReShade32.dll", b"32")], &[b'M', b'Z', 0, 0, 0, 0, 0, 0]);
        assert_eq!(install_reshade_from_setup(&setup, t.path(), 64).unwrap(), vec!["dxgi.dll"]);
        assert!(game::inspect(&exe).unwrap().reshade);
    }

    #[test]
    fn lumenite_zip_places_shaders_includes_texture_and_ignores_slip() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let z = t.path().join("LumeniteFX.zip");
        write_zip(&z, &[
            ("LumeniteFX-mainline/README.md", b"x"),
            ("LumeniteFX-mainline/Shaders/lumenite_Kernel.fx", b"technique Lumenite_Kernel {}"),
            ("LumeniteFX-mainline/Shaders/lumenite_TRAA.fx", b"t"),
            ("LumeniteFX-mainline/Shaders/include/lumenite_Helpers.fxh", b"h"),
            ("LumeniteFX-mainline/Textures/lumenite_bluenoise256.png", b"png"),
            ("../evil.fx", b"zip-slip"),
        ], &[]);
        let installed = install_lumenite_from_zip(&z, t.path()).unwrap();
        assert_eq!(installed.len(), 4);
        assert!(t.path().join("reshade-shaders/Shaders/lumenite_Kernel.fx").is_file());
        assert!(t.path().join("reshade-shaders/Shaders/include/lumenite_Helpers.fxh").is_file());
        assert!(t.path().join("reshade-shaders/Textures/lumenite_bluenoise256.png").is_file());
        assert!(!t.path().parent().unwrap().join("evil.fx").exists());
        assert!(game::inspect(&exe).unwrap().lumenite);

        let bad = t.path().join("bad.zip");
        write_zip(&bad, &[("whatever.txt", b"x")], &[]);
        assert!(install_lumenite_from_zip(&bad, t.path()).is_err());
    }

    #[test]
    fn single_from_zip_and_uninstall() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let z = t.path().join("renodx-dlss5-4.55.zip");
        write_zip(&z, &[("renodx-dlss5.addon64", b"addon")], &[]);
        install_single_from_zip(&z, "renodx-dlss5.addon64", &t.path().join("renodx-dlss5.addon64")).unwrap();
        assert!(game::inspect(&exe).unwrap().dlss5_addon);
        fs::write(t.path().join(game::DLSS_DLL), b"keep").unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.contains(&"renodx-dlss5.addon64".to_string()));
        assert!(t.path().join(game::DLSS_DLL).is_file());
    }

    #[test]
    fn run_all_refuses_32bit_before_network() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let err = run_all(&exe, &|_, _| {}, &|_, _, _, _| {}).unwrap_err();
        assert!(err.to_string().contains("32-bit"));
    }
}
