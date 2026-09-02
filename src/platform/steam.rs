//! Steam on Linux: find installs, enumerate Proton games, map each game to
//! its Proton build, and read/write per-game launch options.
//!
//! Everything is plain files: `steamapps/libraryfolders.vdf` lists the
//! libraries, `steamapps/appmanifest_<appid>.acf` describes each game,
//! `config/config.vdf` holds the CompatToolMapping (which Proton a game uses),
//! and `userdata/<uid>/config/localconfig.vdf` holds LaunchOptions. Parsing is
//! platform-neutral (fixture-tested everywhere); only the probes for real
//! system paths are Linux-gated.

use super::launch_options::{self, LaunchReq};
use super::vdf;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamGame {
    pub appid: String,
    pub name: String,
    /// The game's install folder (steamapps/common/<installdir>).
    pub dir: PathBuf,
    /// The library holding it (contains steamapps/).
    pub library: PathBuf,
    /// The Steam root it was found through (has config/ and userdata/).
    pub root: PathBuf,
}

/// Steam roots that exist on this system (native, flatpak, snap), deduplicated
/// (`~/.steam/steam` is usually a symlink to `~/.local/share/Steam`).
#[cfg(target_os = "linux")]
pub fn roots() -> Vec<PathBuf> {
    match std::env::var_os("HOME") {
        Some(h) => roots_from(Path::new(&h)),
        None => Vec::new(),
    }
}

pub fn roots_from(home: &Path) -> Vec<PathBuf> {
    let candidates = [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join("snap/steam/common/.local/share/Steam"),
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if !c.join("steamapps").is_dir() {
            continue;
        }
        let canon = c.canonicalize().unwrap_or(c);
        if !out.contains(&canon) {
            out.push(canon);
        }
    }
    out
}

/// All library folders of a root, the root's own steamapps included.
pub fn libraries(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    if let Ok(text) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
        if let Ok(tree) = vdf::parse(&text) {
            if let Some(vdf::Value::Block(folders)) = tree.get_ci("libraryfolders") {
                for (_, v) in &folders.0 {
                    if let vdf::Value::Block(b) = v {
                        if let Some(p) = b.string_at(&["path"]) {
                            let p = PathBuf::from(p);
                            let canon = p.canonicalize().unwrap_or(p);
                            if canon.join("steamapps").is_dir() && !out.contains(&canon) {
                                out.push(canon);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Rows that live in steamapps but are not games.
fn is_tooling(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("proton")
        || n.starts_with("steam linux runtime")
        || n.starts_with("steamworks common")
        || n.starts_with("steamvr")
}

pub fn games(root: &Path) -> Vec<SteamGame> {
    let mut out = Vec::new();
    for lib in libraries(root) {
        let steamapps = lib.join("steamapps");
        let Ok(rd) = fs::read_dir(&steamapps) else {
            continue;
        };
        for e in rd.flatten() {
            let fname = e.file_name();
            let n = fname.to_string_lossy();
            if !n.starts_with("appmanifest_") || !n.ends_with(".acf") {
                continue;
            }
            let Ok(text) = fs::read_to_string(e.path()) else {
                continue;
            };
            let Ok(tree) = vdf::parse(&text) else {
                continue;
            };
            let appid = tree.string_at(&["AppState", "appid"]);
            let name = tree.string_at(&["AppState", "name"]);
            let installdir = tree.string_at(&["AppState", "installdir"]);
            let (Some(appid), Some(name), Some(installdir)) = (appid, name, installdir) else {
                continue;
            };
            if is_tooling(name) {
                continue;
            }
            let dir = steamapps.join("common").join(installdir);
            if !dir.is_dir() {
                continue;
            }
            out.push(SteamGame {
                appid: appid.to_string(),
                name: name.to_string(),
                dir,
                library: lib.clone(),
                root: root.to_path_buf(),
            });
        }
    }
    out.sort_by_key(|g| g.name.to_lowercase());
    out
}

/// The game's Proton prefix parent (…/steamapps/compatdata/<appid>), if it exists.
#[allow(dead_code)] // wired into diagnose's host findings
pub fn compatdata(g: &SteamGame) -> Option<PathBuf> {
    let p = g.library.join("steamapps/compatdata").join(&g.appid);
    p.is_dir().then_some(p)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonInfo {
    /// The CompatToolMapping name, e.g. "proton_9", "GE-Proton11-5-x86_64".
    pub raw: String,
    pub major: Option<u32>,
}

/// Which Proton the game is mapped to: its own CompatToolMapping entry, else
/// the global default ("0"), else None (Steam's unversioned default).
pub fn proton_for(root: &Path, appid: &str) -> Option<ProtonInfo> {
    let text = fs::read_to_string(root.join("config/config.vdf")).ok()?;
    let tree = vdf::parse(&text).ok()?;
    let mapping = tree.path_ci(&[
        "InstallConfigStore",
        "Software",
        "Valve",
        "Steam",
        "CompatToolMapping",
    ])?;
    let vdf::Value::Block(mapping) = mapping else {
        return None;
    };
    for key in [appid, "0"] {
        if let Some(vdf::Value::Block(b)) = mapping.get_ci(key) {
            if let Some(name) = b.string_at(&["name"]) {
                if !name.is_empty() {
                    return Some(proton_info(name));
                }
            }
        }
    }
    None
}

pub fn proton_info(raw: &str) -> ProtonInfo {
    let lower = raw.to_ascii_lowercase();
    // Valve's legacy scheme packs "X.Y" into the digits: proton_63 is 6.3.
    for (name, major) in [
        ("proton_316", 3u32),
        ("proton_37", 3),
        ("proton_411", 4),
        ("proton_42", 4),
        ("proton_5", 5),
        ("proton_513", 5),
        ("proton_63", 6),
        ("proton_7", 7),
        ("proton_8", 8),
    ] {
        if lower == name {
            return ProtonInfo {
                raw: raw.to_string(),
                major: Some(major),
            };
        }
    }
    // Otherwise: first run of digits after the word "proton" (GE-Proton11-5,
    // proton-cachyos-11.0-…, UMU-Proton-10.0-4, proton_9).
    let major = lower.find("proton").and_then(|at| {
        let rest = &lower[at + "proton".len()..];
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    });
    ProtonInfo {
        raw: raw.to_string(),
        major,
    }
}

/// Whether `PROTON_ENABLE_NVAPI=1` should be set: NVAPI is default-on since
/// Proton 9 (Experimental/Hotfix are always current). Unknown ⇒ include it —
/// it is harmless when redundant.
pub fn nvapi_env_needed(p: Option<&ProtonInfo>) -> bool {
    let Some(p) = p else { return true };
    let raw = p.raw.to_ascii_lowercase();
    if raw.contains("experimental") || raw.contains("hotfix") {
        return false;
    }
    match p.major {
        Some(m) => m < 9,
        None => true,
    }
}

/// Is the Steam client running? Editing localconfig.vdf under it is futile —
/// Steam keeps the file in memory and rewrites it on exit.
#[cfg(target_os = "linux")]
pub fn is_running() -> bool {
    let Ok(rd) = fs::read_dir("/proc") else {
        return false;
    };
    for e in rd.flatten() {
        if let Ok(comm) = fs::read_to_string(e.path().join("comm")) {
            if comm.trim() == "steam" {
                return true;
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
pub fn is_running() -> bool {
    false
}

const LC_PATH: [&str; 5] = ["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"];

fn localconfigs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(root.join("userdata")) {
        for e in rd.flatten() {
            let p = e.path().join("config/localconfig.vdf");
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

fn lo_path(appid: &str) -> Vec<&str> {
    let mut p: Vec<&str> = LC_PATH.to_vec();
    p.push(appid);
    p.push("LaunchOptions");
    p
}

/// Current LaunchOptions per localconfig.vdf (one entry per Steam user).
#[allow(dead_code)] // wired into diagnose's host findings
pub fn read_launch_options(root: &Path, appid: &str) -> Vec<(PathBuf, Option<String>)> {
    localconfigs(root)
        .into_iter()
        .map(|p| {
            let cur = fs::read_to_string(&p)
                .ok()
                .and_then(|t| vdf::parse(&t).ok())
                .and_then(|tree| tree.string_at(&lo_path(appid)).map(str::to_string));
            (p, cur)
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    pub file: PathBuf,
    pub backup: PathBuf,
    pub merged: String,
}

/// Merge the required options into the game's LaunchOptions. Refuses while
/// Steam runs. See `apply_with` for the mechanics.
pub fn apply_launch_options(
    root: &Path,
    appid: &str,
    req: &LaunchReq,
) -> Result<Vec<ApplyOutcome>> {
    apply_with(root, appid, req, is_running(), false)
}

pub fn revert_launch_options(
    root: &Path,
    appid: &str,
    req: &LaunchReq,
) -> Result<Vec<ApplyOutcome>> {
    apply_with(root, appid, req, is_running(), true)
}

/// The guarded edit: for each Steam user's localconfig.vdf (the appid block is
/// created only in the most recently modified one; others are edited only when
/// they already know the game), back the file up (`.dlss5o.orig` once ever,
/// `.dlss5o.bak` per edit), splice the one value with
/// `vdf::set_string_preserving`, verify the result re-parses into the same
/// tree apart from that value, and swap it in atomically. Any failure leaves
/// the original file untouched.
pub fn apply_with(
    root: &Path,
    appid: &str,
    req: &LaunchReq,
    steam_running: bool,
    revert: bool,
) -> Result<Vec<ApplyOutcome>> {
    if steam_running {
        bail!(
            "Steam is running — it would overwrite the change on exit. \
             Close Steam and retry, or paste the options into the game's Properties yourself."
        );
    }
    let files = localconfigs(root);
    if files.is_empty() {
        bail!("no userdata/<uid>/config/localconfig.vdf under {}", root.display());
    }
    let primary = files
        .iter()
        .max_by_key(|p| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        })
        .cloned();
    let path = lo_path(appid);
    let mut out = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        let tree = vdf::parse(&text)
            .with_context(|| format!("cannot parse {}", file.display()))?;
        let existing = tree.string_at(&path).unwrap_or("");
        let knows_game = {
            let mut app_block: Vec<&str> = LC_PATH.to_vec();
            app_block.push(appid);
            tree.path_ci(&app_block).is_some()
        };
        if !knows_game && Some(&file) != primary.as_ref() {
            continue;
        }
        let merged = if revert {
            launch_options::strip(existing, req)
        } else {
            launch_options::merge(existing, req)
        };
        if merged == existing {
            continue;
        }
        let new_text = vdf::set_string_preserving(&text, &path, &merged)
            .with_context(|| format!("cannot edit {}", file.display()))?;
        let new_tree = vdf::parse(&new_text).context("edited file does not re-parse")?;
        if new_tree.string_at(&path) != Some(merged.as_str())
            || !vdf::equal_except(&tree, &new_tree, &path)
        {
            bail!(
                "edit self-check failed for {} — file left untouched",
                file.display()
            );
        }
        let orig = file.with_extension("vdf.dlss5o.orig");
        if !orig.exists() {
            fs::copy(&file, &orig)?;
        }
        let bak = file.with_extension("vdf.dlss5o.bak");
        fs::copy(&file, &bak)?;
        let tmp = file.with_extension("vdf.dlss5o.tmp");
        fs::write(&tmp, &new_text)?;
        fs::rename(&tmp, &file)?;
        out.push(ApplyOutcome {
            file,
            backup: bak,
            merged,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_root(home: &Path) -> PathBuf {
        let root = home.join(".local/share/Steam");
        fs::create_dir_all(root.join("steamapps/common")).unwrap();
        root
    }

    fn write(p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn manifest(appid: &str, name: &str, installdir: &str) -> String {
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"{name}\"\n\t\"installdir\"\t\t\"{installdir}\"\n}}\n"
        )
    }

    #[test]
    fn roots_from_probes_and_dedupes() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let native = fake_root(home);
        // ~/.steam/steam as symlink to the native root must collapse to one.
        #[cfg(unix)]
        {
            fs::create_dir_all(home.join(".steam")).unwrap();
            std::os::unix::fs::symlink(&native, home.join(".steam/steam")).unwrap();
        }
        fs::create_dir_all(
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps"),
        )
        .unwrap();
        let got = roots_from(home);
        assert_eq!(got.len(), 2, "{got:?}");
    }

    #[test]
    fn libraries_and_games_from_fixtures() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let root = fake_root(home);
        let lib2 = home.join("ssd/SteamLibrary");
        fs::create_dir_all(lib2.join("steamapps/common/Foo Game")).unwrap();
        write(
            &root.join("steamapps/libraryfolders.vdf"),
            &format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                root.display(),
                lib2.display()
            ),
        );
        write(
            &lib2.join("steamapps/appmanifest_42.acf"),
            &manifest("42", "Foo Game", "Foo Game"),
        );
        // Tooling rows and games without folders are filtered out.
        fs::create_dir_all(root.join("steamapps/common/Proton 11.0")).unwrap();
        write(
            &root.join("steamapps/appmanifest_1.acf"),
            &manifest("1", "Proton 11.0", "Proton 11.0"),
        );
        write(
            &root.join("steamapps/appmanifest_2.acf"),
            &manifest("2", "Gone Game", "not-on-disk"),
        );
        let libs = libraries(&root);
        assert_eq!(libs.len(), 2);
        let games = games(&root);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].appid, "42");
        assert_eq!(games[0].dir, lib2.join("steamapps/common/Foo Game"));
    }

    #[test]
    fn proton_versions_parse() {
        for (raw, major) in [
            ("proton_9", Some(9)),
            ("proton_experimental", None),
            ("GE-Proton10-34", Some(10)),
            ("GE-Proton11-5-x86_64", Some(11)),
            ("proton-cachyos-11.0-20260703-slr-x86_64_v3", Some(11)),
            ("UMU-Proton-10.0-4", Some(10)),
            ("proton_63", Some(6)),
            ("proton_513", Some(5)),
            ("proton_7", Some(7)),
            ("garbage", None),
        ] {
            assert_eq!(proton_info(raw).major, major, "{raw}");
        }
    }

    #[test]
    fn nvapi_needed_matrix() {
        assert!(nvapi_env_needed(None));
        assert!(nvapi_env_needed(Some(&proton_info("proton_63"))));
        assert!(nvapi_env_needed(Some(&proton_info("proton_8"))));
        assert!(!nvapi_env_needed(Some(&proton_info("proton_9"))));
        assert!(!nvapi_env_needed(Some(&proton_info("GE-Proton11-5"))));
        assert!(!nvapi_env_needed(Some(&proton_info("proton_experimental"))));
        assert!(nvapi_env_needed(Some(&proton_info("garbage"))));
    }

    #[test]
    fn proton_for_reads_mapping_and_default() {
        let t = tempfile::tempdir().unwrap();
        let root = fake_root(t.path());
        write(
            &root.join("config/config.vdf"),
            "\"InstallConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"CompatToolMapping\"\n\t\t\t\t{\n\t\t\t\t\t\"0\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"name\"\t\t\"proton_experimental\"\n\t\t\t\t\t}\n\t\t\t\t\t\"489830\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"name\"\t\t\"GE-Proton11-5-x86_64\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n",
        );
        assert_eq!(
            proton_for(&root, "489830").unwrap().raw,
            "GE-Proton11-5-x86_64"
        );
        assert_eq!(
            proton_for(&root, "1091500").unwrap().raw,
            "proton_experimental"
        );
    }

    fn sample_localconfig() -> String {
        "\"UserLocalConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"489830\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"Playtime\"\t\t\"7\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n"
            .to_string()
    }

    fn dxgi_req() -> LaunchReq {
        LaunchReq {
            overrides: vec![("dxgi".into(), "n,b".into())],
            env: vec![("PROTON_ENABLE_NVAPI".into(), "1".into())],
        }
    }

    #[test]
    fn apply_writes_backups_and_is_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let root = fake_root(t.path());
        let lc = root.join("userdata/111/config/localconfig.vdf");
        write(&lc, &sample_localconfig());
        let req = dxgi_req();

        let out = apply_with(&root, "489830", &req, false, false).unwrap();
        assert_eq!(out.len(), 1);
        assert!(lc.with_extension("vdf.dlss5o.orig").is_file());
        assert!(lc.with_extension("vdf.dlss5o.bak").is_file());
        let now = read_launch_options(&root, "489830");
        assert_eq!(
            now[0].1.as_deref(),
            Some("WINEDLLOVERRIDES=\"dxgi=n,b\" PROTON_ENABLE_NVAPI=1 %command%")
        );
        // Untouched sibling data survives.
        let text = fs::read_to_string(&lc).unwrap();
        assert!(text.contains("\"Playtime\"\t\t\"7\""));

        // Second apply: nothing to do, no new outcome.
        let out2 = apply_with(&root, "489830", &req, false, false).unwrap();
        assert!(out2.is_empty());

        // Revert restores an empty options string.
        let out3 = apply_with(&root, "489830", &req, false, true).unwrap();
        assert_eq!(out3.len(), 1);
        assert_eq!(read_launch_options(&root, "489830")[0].1.as_deref(), Some(""));
    }

    #[test]
    fn apply_refuses_while_steam_runs() {
        let t = tempfile::tempdir().unwrap();
        let root = fake_root(t.path());
        write(
            &root.join("userdata/111/config/localconfig.vdf"),
            &sample_localconfig(),
        );
        assert!(apply_with(&root, "489830", &dxgi_req(), true, false).is_err());
    }

    #[test]
    fn apply_only_primary_gets_new_appid_blocks() {
        let t = tempfile::tempdir().unwrap();
        let root = fake_root(t.path());
        let old = root.join("userdata/111/config/localconfig.vdf");
        let newer = root.join("userdata/222/config/localconfig.vdf");
        write(&old, &sample_localconfig());
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&newer, &sample_localconfig());
        // appid 999 exists in neither: only the newest file gains it.
        let out = apply_with(&root, "999", &dxgi_req(), false, false).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].file, newer);
        // 489830 exists in both: both are updated.
        let out = apply_with(&root, "489830", &dxgi_req(), false, false).unwrap();
        assert_eq!(out.len(), 2);
    }
}
