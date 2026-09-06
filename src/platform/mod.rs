//! Linux game-launcher integration: discovery across Steam, Heroic and Lutris,
//! plus the launch-option plumbing a Proton game needs. Parsers and mergers are
//! platform-neutral (and tested on every OS); the functions that probe real
//! system paths are Linux-only.

pub mod heroic;
pub mod launch_options;
pub mod lutris;
pub mod steam;
pub mod vdf;

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    Steam,
    Heroic,
    Lutris,
}

impl Launcher {
    pub fn label(self) -> &'static str {
        match self {
            Launcher::Steam => "Steam",
            Launcher::Heroic => "Heroic",
            Launcher::Lutris => "Lutris",
        }
    }
}

/// One installed game found through a launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEntry {
    pub launcher: Launcher,
    pub name: String,
    /// Steam appid / Heroic app_name / Lutris slug.
    pub id: String,
    /// The game's install folder — what feeds `game::resolve_target`.
    pub dir: PathBuf,
    /// The owning launcher root (Steam root, Heroic config root; empty for Lutris).
    pub root: PathBuf,
}

/// Every game the supported launchers know about, Steam first.
#[cfg(target_os = "linux")]
pub fn scan_all() -> Vec<GameEntry> {
    let mut out = Vec::new();
    for root in steam::roots() {
        for g in steam::games(&root) {
            out.push(GameEntry {
                launcher: Launcher::Steam,
                name: g.name,
                id: g.appid,
                dir: g.dir,
                root: g.root,
            });
        }
    }
    for root in heroic::roots() {
        for g in heroic::games(&root) {
            out.push(GameEntry {
                launcher: Launcher::Heroic,
                name: g.name,
                id: g.app_name,
                dir: g.dir,
                root: root.clone(),
            });
        }
    }
    if let Some(data) = lutris::data_dir() {
        for g in lutris::games_from(&data) {
            if let Some(dir) = g.dir {
                out.push(GameEntry {
                    launcher: Launcher::Lutris,
                    name: g.name,
                    id: g.slug,
                    dir,
                    root: PathBuf::new(),
                });
            }
        }
    }
    out
}

#[cfg(not(target_os = "linux"))]
pub fn scan_all() -> Vec<GameEntry> {
    Vec::new()
}

/// What happened (or what the user must do) about a game's launch options
/// after an install or on an explicit request. Rendered by both CLI and GUI.
#[derive(Debug, Clone)]
pub enum LaunchAdvice {
    /// Steam localconfig.vdf was edited; one outcome per Steam user updated.
    AppliedSteam(Vec<steam::ApplyOutcome>),
    /// Everything required was already there.
    AlreadySet,
    /// Steam could not be edited (running, parse trouble, …): show the exact
    /// string to paste into Properties → Launch Options, and why.
    ManualSteam { display: String, why: String },
    /// Heroic's per-game config was updated.
    AppliedHeroic { file: PathBuf },
    /// Set these env vars in the launcher's own game settings (Lutris always;
    /// Heroic when its config could not be edited).
    ManualEnv {
        launcher: Launcher,
        vars: Vec<(String, String)>,
        why: Option<String>,
    },
    /// The folder belongs to no known launcher: generic Steam-style string.
    UnknownLauncher { display: String },
}

/// Decide and (where safe) apply the launch options a freshly set-up game
/// needs. `revert` removes exactly what apply would add.
pub fn ensure_launch_options(
    game_dir: &Path,
    engine: crate::installer::Engine,
    revert: bool,
) -> LaunchAdvice {
    use launch_options as lo;
    let entry = entry_for_path(game_dir);
    let proton = entry.as_ref().and_then(|e| {
        (e.launcher == Launcher::Steam).then(|| steam::proton_for(&e.root, &e.id))?
    });
    let req = lo::required(game_dir, engine, proton.as_ref());
    let Some(entry) = entry else {
        return LaunchAdvice::UnknownLauncher {
            display: lo::display(&req),
        };
    };
    match entry.launcher {
        Launcher::Steam => {
            let r = if revert {
                steam::revert_launch_options(&entry.root, &entry.id, &req)
            } else {
                steam::apply_launch_options(&entry.root, &entry.id, &req)
            };
            match r {
                Ok(outcomes) if outcomes.is_empty() => LaunchAdvice::AlreadySet,
                Ok(outcomes) => LaunchAdvice::AppliedSteam(outcomes),
                Err(e) => LaunchAdvice::ManualSteam {
                    display: lo::display(&req),
                    why: format!("{e:#}"),
                },
            }
        }
        Launcher::Heroic => {
            if revert {
                return LaunchAdvice::ManualEnv {
                    launcher: Launcher::Heroic,
                    vars: lo::env_pairs(&req),
                    why: Some("remove these from the game's Heroic settings".into()),
                };
            }
            match heroic::apply_env(&entry.root, &entry.id, &req) {
                Ok(file) => LaunchAdvice::AppliedHeroic { file },
                Err(e) => LaunchAdvice::ManualEnv {
                    launcher: Launcher::Heroic,
                    vars: lo::env_pairs(&req),
                    why: Some(format!("{e:#}")),
                },
            }
        }
        Launcher::Lutris => LaunchAdvice::ManualEnv {
            launcher: Launcher::Lutris,
            vars: lo::env_pairs(&req),
            why: None,
        },
    }
}

/// What happened (or must happen) about the Microsoft `d3dcompiler_47.dll` the
/// RenoDX DLSS 5 add-on needs under Proton. The add-on compiles its neural
/// pass at runtime through `d3dcompiler_47`; Wine's builtin one is backed by
/// vkd3d-shader's still-incomplete HLSL compiler and does not implement every
/// intrinsic it uses (`isnan`), so the shader fails to compile and neural
/// rendering never binds. Microsoft's real DLL knows the intrinsic.
#[derive(Debug, Clone)]
pub enum D3dcompilerAdvice {
    /// Not a Steam/Proton game, or the add-on is not installed: nothing to do.
    NotApplicable,
    /// A real (Microsoft) `d3dcompiler_47.dll` is already in the prefix.
    AlreadyPresent,
    /// It was just installed into the prefix (by `protontricks`/`winetricks`).
    Installed { via: String },
    /// The installer ran but failed; show the reason and the manual command.
    Failed { cmd: String, why: String },
    /// No `protontricks`/`winetricks` on `PATH`: show the exact command to run.
    Manual { cmd: String },
}

/// Microsoft's redistributable `d3dcompiler_47.dll` is ~4–5 MB; Wine's builtin
/// is ~0.4 MB. Anything under this (or absent) is the builtin, which is the one
/// that cannot compile the add-on's pass.
#[cfg(target_os = "linux")]
const D3DCOMPILER_REAL_MIN: u64 = 1_000_000;

/// Ensure the game's Proton prefix has Microsoft's `d3dcompiler_47.dll` so the
/// DLSS 5 add-on's neural pass can compile. Runs `protontricks`/`winetricks`
/// when one is on `PATH`; otherwise returns the exact command for the user.
/// Only acts for the ReShade engine (the one that installs the add-on) on a
/// Steam game under Proton. `progress` is called for the slow install.
#[cfg(target_os = "linux")]
pub fn ensure_d3dcompiler(
    game_dir: &Path,
    engine: crate::installer::Engine,
    progress: &dyn Fn(&str),
) -> D3dcompilerAdvice {
    use D3dcompilerAdvice as A;
    // Only the ReShade engine installs the add-on that compiles a runtime pass;
    // OptiScaler carries its own upscaler and needs no d3dcompiler.
    if engine != crate::installer::Engine::ReShade
        || !game_dir.join(crate::game::DLSS5_ADDON).is_file()
    {
        return A::NotApplicable;
    }
    let Some(entry) = entry_for_path(game_dir) else {
        return A::NotApplicable;
    };
    if entry.launcher != Launcher::Steam {
        return A::NotApplicable;
    }
    // A game not mapped to Proton needs no Wine d3dcompiler at all.
    if steam::proton_for(&entry.root, &entry.id).is_none() {
        return A::NotApplicable;
    }
    let g = steam::SteamGame {
        appid: entry.id.clone(),
        name: entry.name.clone(),
        dir: entry.dir.clone(),
        library: entry
            .dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .unwrap_or(&entry.root)
            .to_path_buf(),
        root: entry.root.clone(),
    };
    let Some(cd) = steam::compatdata(&g) else {
        return A::NotApplicable;
    };
    let sys32 = cd.join("pfx/drive_c/windows/system32/d3dcompiler_47.dll");
    let real = std::fs::metadata(&sys32)
        .map(|m| m.len() >= D3DCOMPILER_REAL_MIN)
        .unwrap_or(false);
    if real {
        return A::AlreadyPresent;
    }
    let cmd = format!("protontricks {} d3dcompiler_47", entry.id);
    // protontricks knows the Steam layout and points Wine at the right prefix.
    if which("protontricks") {
        progress("Installing d3dcompiler_47 into the Proton prefix (this can take a minute)");
        match run_winetricks_d3dcompiler(WinetricksKind::Protontricks(&entry.id)) {
            Ok(()) if std::fs::metadata(&sys32).map(|m| m.len() >= D3DCOMPILER_REAL_MIN).unwrap_or(false) => {
                A::Installed { via: "protontricks".into() }
            }
            Ok(()) => A::Failed {
                cmd,
                why: "protontricks finished but the prefix still holds the builtin DLL".into(),
            },
            Err(e) => A::Failed { cmd, why: e },
        }
    } else if which("winetricks") {
        progress("Installing d3dcompiler_47 into the Proton prefix (this can take a minute)");
        let pfx = cd.join("pfx");
        match run_winetricks_d3dcompiler(WinetricksKind::Winetricks(&pfx)) {
            Ok(()) if std::fs::metadata(&sys32).map(|m| m.len() >= D3DCOMPILER_REAL_MIN).unwrap_or(false) => {
                A::Installed { via: "winetricks".into() }
            }
            Ok(()) => A::Failed {
                cmd,
                why: "winetricks finished but the prefix still holds the builtin DLL".into(),
            },
            Err(e) => A::Failed { cmd, why: e },
        }
    } else {
        A::Manual { cmd }
    }
}

#[cfg(target_os = "linux")]
enum WinetricksKind<'a> {
    Protontricks(&'a str),
    Winetricks(&'a Path),
}

/// Run the d3dcompiler_47 verb unattended and return a short error on failure.
#[cfg(target_os = "linux")]
fn run_winetricks_d3dcompiler(kind: WinetricksKind) -> Result<(), String> {
    use std::process::Command;
    let mut cmd = match kind {
        WinetricksKind::Protontricks(appid) => {
            let mut c = Command::new("protontricks");
            c.arg(appid).arg("-q").arg("d3dcompiler_47");
            c
        }
        WinetricksKind::Winetricks(pfx) => {
            let mut c = Command::new("winetricks");
            c.env("WINEPREFIX", pfx).arg("-q").arg("d3dcompiler_47");
            c
        }
    };
    // Non-interactive: never let the tool block on a prompt.
    cmd.env("WINETRICKS_GUI", "none");
    cmd.stdin(std::process::Stdio::null());
    let out = cmd
        .output()
        .map_err(|e| format!("could not run: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let tail = String::from_utf8_lossy(&out.stderr);
    let tail = tail
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output");
    Err(format!(
        "exit {}: {tail}",
        out.status.code().unwrap_or(-1)
    ))
}

/// Is a program on `PATH`?
#[cfg(target_os = "linux")]
fn which(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|p| p.join(bin).is_file())
    })
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_d3dcompiler(
    _game_dir: &Path,
    _engine: crate::installer::Engine,
    _progress: &dyn Fn(&str),
) -> D3dcompilerAdvice {
    D3dcompilerAdvice::NotApplicable
}

/// Candidate locations for the NVIDIA driver's Wine NGX DLLs across distros.
#[cfg(target_os = "linux")]
const NVNGX_WINE_DIRS: [&str; 4] = [
    "/usr/lib/nvidia/wine",                 // Arch and family
    "/usr/lib64/nvidia/wine",               // Fedora/openSUSE
    "/usr/lib/x86_64-linux-gnu/nvidia/wine", // Debian/Ubuntu
    "/usr/lib/extra/nvidia/wine",
];

/// Where the NVIDIA driver's Wine NGX DLLs live on this system, if anywhere —
/// the Linux equivalent of Windows' NGX Core presence.
#[cfg(target_os = "linux")]
pub fn nvngx_wine_dir() -> Option<std::path::PathBuf> {
    NVNGX_WINE_DIRS
        .iter()
        .map(Path::new)
        .find(|d| d.join("nvngx.dll").is_file())
        .map(Path::to_path_buf)
}

/// Assemble the Linux facts `diagnose::host_findings` reports on. Cheap, pure
/// reads; anything unknown stays None/empty.
#[cfg(target_os = "linux")]
pub fn host_context(st: &crate::game::GameStatus) -> crate::diagnose::HostContext {
    use crate::diagnose::HostContext;
    use launch_options as lo;
    let game_dir = st.game_dir();
    let engine = if st.opti {
        crate::installer::Engine::Opti
    } else {
        crate::installer::Engine::ReShade
    };
    let entry = entry_for_path(game_dir);
    let mut ctx = HostContext {
        relevant: true,
        launcher: entry.as_ref().map(|e| e.launcher.label()),
        nvngx_wine_dir: nvngx_wine_dir(),
        driver_version: crate::gpu::driver_version(),
        steam_running: steam::is_running(),
        mfg_installed: st.mfg,
        ..HostContext::default()
    };
    let proton = entry.as_ref().and_then(|e| {
        (e.launcher == Launcher::Steam).then(|| steam::proton_for(&e.root, &e.id))?
    });
    let req = lo::required(game_dir, engine, proton.as_ref());
    ctx.required_display = lo::display(&req);
    ctx.proton = proton.as_ref().map(|p| p.raw.clone());
    ctx.proton_needs_nvapi_env = proton.is_some() && steam::nvapi_env_needed(proton.as_ref());
    ctx.d3dcompiler_missing_feeder = !lo::d3dcompiler_present(game_dir);
    if let Some(e) = &entry {
        if e.launcher == Launcher::Steam {
            ctx.steam_options = steam::read_launch_options(&e.root, &e.id)
                .into_iter()
                .map(|(file, cur)| {
                    let cur = cur.unwrap_or_default();
                    let label = file
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.file_name())
                        .map(|u| format!("user {}", u.to_string_lossy()))
                        .unwrap_or_else(|| file.display().to_string());
                    (label, lo::merge(&cur, &req) == cur)
                })
                .collect();
            let g = steam::SteamGame {
                appid: e.id.clone(),
                name: e.name.clone(),
                dir: e.dir.clone(),
                library: e
                    .dir
                    .parent() // common/
                    .and_then(|p| p.parent()) // steamapps/
                    .and_then(|p| p.parent()) // library
                    .unwrap_or(&e.root)
                    .to_path_buf(),
                root: e.root.clone(),
            };
            if let Some(cd) = steam::compatdata(&g) {
                ctx.prefix_nvngx =
                    Some(cd.join("pfx/drive_c/windows/system32/nvngx.dll").is_file());
                if st.mfg {
                    ctx.mfg_log_tail = newest_mfg_log_tail(&cd);
                }
            }
        }
    }
    ctx
}

/// The last meaningful line of the newest `MfgUnlock-*.log` the mod wrote into
/// the game's Proton prefix TEMP — proof its ASI loaded under Proton, and the
/// single best signal of whether the unlock actually engaged.
#[cfg(target_os = "linux")]
fn newest_mfg_log_tail(compatdata: &Path) -> Option<String> {
    let temp = compatdata.join("pfx/drive_c/users/steamuser/Temp");
    let mut logs: Vec<std::path::PathBuf> = std::fs::read_dir(&temp)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("MfgUnlock-") && n.ends_with(".log"))
        })
        .collect();
    logs.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    let newest = logs.last()?;
    let text = std::fs::read_to_string(newest).ok()?;
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

#[cfg(not(target_os = "linux"))]
pub fn host_context(_st: &crate::game::GameStatus) -> crate::diagnose::HostContext {
    crate::diagnose::HostContext::default()
}

/// Map a manually chosen path back to the launcher entry that owns it, so the
/// launch-option handling also works when the user picked a folder by hand.
pub fn entry_for_path(p: &Path) -> Option<GameEntry> {
    let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    scan_all()
        .into_iter()
        .filter(|e| {
            let dir = e.dir.canonicalize().unwrap_or_else(|_| e.dir.clone());
            p.starts_with(&dir)
        })
        // The deepest matching dir wins (nested library layouts).
        .max_by_key(|e| e.dir.components().count())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn which_finds_real_binaries_and_rejects_fake_ones() {
        // `sh` is on PATH on every Linux box the tool runs on; a random name is not.
        assert!(which("sh"));
        assert!(!which("dlss5oneclick-no-such-binary-42"));
    }

    #[test]
    fn ensure_d3dcompiler_is_noop_off_steam() {
        // A folder that belongs to no launcher (a bare tempdir) with no add-on:
        // nothing to provision, and nothing is run.
        let t = tempfile::tempdir().unwrap();
        let adv = ensure_d3dcompiler(t.path(), crate::installer::Engine::ReShade, &|_| {});
        assert!(matches!(adv, D3dcompilerAdvice::NotApplicable));
    }
}
