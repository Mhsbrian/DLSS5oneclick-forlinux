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

/// Candidate locations for the NVIDIA driver's Wine NGX DLLs across distros.
#[cfg(target_os = "linux")]
const NVNGX_WINE_DIRS: [&str; 4] = [
    "/usr/lib/nvidia/wine",                 // Arch and family
    "/usr/lib64/nvidia/wine",               // Fedora/openSUSE
    "/usr/lib/x86_64-linux-gnu/nvidia/wine", // Debian/Ubuntu
    "/usr/lib/extra/nvidia/wine",
];

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
        nvngx_wine_dir: NVNGX_WINE_DIRS
            .iter()
            .map(Path::new)
            .find(|d| d.join("nvngx.dll").is_file())
            .map(Path::to_path_buf),
        driver_version: crate::gpu::driver_version(),
        steam_running: steam::is_running(),
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
            ctx.prefix_nvngx = steam::compatdata(&g).map(|cd| {
                cd.join("pfx/drive_c/windows/system32/nvngx.dll").is_file()
            });
        }
    }
    ctx
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
