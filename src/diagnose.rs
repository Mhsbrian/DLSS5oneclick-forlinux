//! Read the logs a session leaves behind and say why neural rendering is or is
//! not running. Answers the commonest report ("I enabled it, nothing changed")
//! without a round trip: everything needed is already in `ReShade.log` and,
//! on the Feeder path, `dlss5-feed.log` next to the game exe.

use crate::game::{self, GameStatus};
use anyhow::Result;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Bad,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub level: Level,
    pub text: String,
}

fn ok(t: impl Into<String>) -> Finding {
    Finding {
        level: Level::Ok,
        text: t.into(),
    }
}
fn warn(t: impl Into<String>) -> Finding {
    Finding {
        level: Level::Warn,
        text: t.into(),
    }
}
fn bad(t: impl Into<String>) -> Finding {
    Finding {
        level: Level::Bad,
        text: t.into(),
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name)).ok()
}

/// Findings for a game folder, in reading order.
pub fn diagnose(st: &GameStatus) -> Vec<Finding> {
    let d = st.game_dir();
    let mut out = Vec::new();

    // ── ReShade side ────────────────────────────────────────────────
    let Some(rs) = read(d, "ReShade.log").or_else(|| read(d, "ReShade2.log")) else {
        out.push(bad(
            "No ReShade.log next to the game exe: ReShade never loaded. Either the game was not \
             started since the install, or it does not load dxgi.dll (wrong exe picked, or a \
             launcher starts a different one). Check the exe with --check.",
        ));
        return out;
    };
    if rs.contains("Initializing crosire's ReShade") {
        out.push(ok("ReShade loaded into the game."));
    }
    let failed_line = rs
        .lines()
        .find(|l| l.contains("Failed to load add-on") && l.contains("renodx-dlss5"));
    if let Some(l) = failed_line {
        let code = l
            .rsplit("error code ")
            .next()
            .unwrap_or("")
            .trim_end_matches('!');
        let extra = match code.trim() {
            "2148073478" => " (0x80090006 = the process refuses unsigned DLLs; nothing can be done)",
            "1114" => " (the add-on's DLL entry point failed; usually a CPU without AVX2 or a mismatched ReShade version)",
            _ => "",
        };
        out.push(bad(format!(
            "ReShade refused to load renodx-dlss5.addon64, error code {code}{extra}."
        )));
    } else if rs.contains("DLSS 5 Neural Rendering") {
        out.push(ok("The DLSS 5 Neural Rendering add-on registered."));
    } else {
        out.push(bad(
            "The DLSS 5 add-on never registered. renodx-dlss5.addon64 is missing from the game \
             folder, disabled in ReShade's Add-ons tab, or quarantined by antivirus.",
        ));
    }
    if rs.contains("NR toggled ON") && !rs.contains("NR toggled OFF") {
        out.push(ok("Neural rendering was toggled ON (F6)."));
    } else if rs.contains("NR toggled OFF") {
        out.push(warn(
            "The log's last F6 state may be OFF — press F6 in game and watch the add-on's panel.",
        ));
    }
    if rs.contains("inline feature 18 evaluation succeeded") {
        out.push(ok(
            "Neural rendering ran: the add-on evaluated the DLSS 5 model on real frames. If the \
             picture still looks unchanged, raise NR Intensity / Local Structure in its panel — \
             the default is subtle.",
        ));
    } else if rs.contains("feature=1 (DLSS/DLAA)") {
        out.push(warn(
            "The add-on saw the game's DLSS but has not evaluated the model yet (feature 18 never \
             created). Enable DLSS in the game's own graphics settings and enable neural rendering \
             in the add-on panel.",
        ));
    } else if st.mode == game::Mode::Native {
        out.push(bad(
            "No NGX call was intercepted: this game's own DLSS never ran. Turn DLSS on in the \
             game's graphics settings (the add-on hooks the game's DLSS calls; without them it has \
             nothing to work with).",
        ));
    }

    // ── Feeder side (games without DLSS) ────────────────────────────
    if st.mode == game::Mode::Feeder {
        let Some(fd) = read(d, "dlss5-feed.log") else {
            out.push(bad(
                "No dlss5-feed.log: DLSS5-Feeder never started. Its add-on is missing or disabled \
                 in ReShade's Add-ons tab.",
            ));
            return out;
        };
        if fd.contains("feature ready") {
            out.push(ok(
                "DLSS5-Feeder built its DLSS feature (feature ready … DLAA).",
            ));
        }
        if fd.contains("frame") && fd.contains("delivered") {
            out.push(ok("Frames were delivered to the model."));
        }
        if fd.contains("technique MISSING") && !fd.contains("technique found") {
            out.push(bad(
                "DLSS5_Feed.fx is not compiling. Its shader files are missing from \
                 reshade-shaders\\Shaders — re-run Install.",
            ));
        }
        if fd.contains("-> none (not installed)") {
            out.push(bad(
                "The motion-vector provider is not enabled. In ReShade's Home tab enable \
                 \"LUMENITE: Kernel 2.0\" ABOVE \"DLSS5_Feed\", then reload effects.",
            ));
        }
        if fd.contains("MV probe") && fd.contains("0% non-zero") {
            out.push(bad(
                "Motion vectors are all zero: the provider is enabled but writes nothing. Check \
                 that Lumenite_Kernel sits above DLSS5_Feed in the technique list.",
            ));
        }
        if fd.contains("DLSS super sampling is not available") {
            out.push(bad(
                "NGX reported DLSS unavailable. nvngx_dlss.dll must sit next to the game exe — \
                 re-run Install, and make sure antivirus did not remove it.",
            ));
        }
        if fd.contains("stopped:") {
            let line = fd
                .lines()
                .rev()
                .find(|l| l.contains("stopped:"))
                .unwrap_or("")
                .trim()
                .to_string();
            out.push(bad(format!("The feed stopped itself: {line}")));
        }
        if fd.contains("CRASH RECORDED") {
            out.push(warn(
                "The feed recorded a crash inside the DLSS 5 add-on (upstream issue #16). Play in \
                 borderless/windowed rather than exclusive fullscreen, and raise create_delay in \
                 dlss5-feed.cfg.",
            ));
        }
    }
    out
}

/// Read the game folder and produce findings, or a single fatal one.
/// Log-based findings only (host-independent; what the tests cover).
#[cfg(test)]
pub fn run(exe: &Path) -> Result<Vec<Finding>> {
    let st = game::inspect(exe)?;
    Ok(diagnose(&st))
}

/// Host findings (launch options, driver, Proton) first, then the log-based
/// ones — what the CLI and GUI show.
pub fn run_full(exe: &Path) -> Result<Vec<Finding>> {
    let st = game::inspect(exe)?;
    let mut findings = host_findings(&st, &crate::platform::host_context(&st));
    findings.extend(diagnose(&st));
    Ok(findings)
}

/// Linux-side facts about how this game is launched, gathered by
/// `platform::host_context`. Everything defaults to "unknown/irrelevant", so
/// on Windows (or when nothing is known) `host_findings` stays silent and the
/// log-based diagnosis above is all there is.
#[derive(Debug, Clone, Default)]
pub struct HostContext {
    /// False ⇒ produce no host findings at all.
    pub relevant: bool,
    /// "Steam" / "Heroic" / "Lutris" when the game folder maps to a launcher.
    pub launcher: Option<&'static str>,
    /// Per Steam user file: (short label, launch options already satisfy the
    /// requirements?). Empty for non-Steam launchers.
    pub steam_options: Vec<(String, bool)>,
    /// The full string to paste when something is missing.
    pub required_display: String,
    /// CompatToolMapping name, e.g. "GE-Proton11-5-x86_64".
    pub proton: Option<String>,
    /// The Proton build predates default-on NVAPI (or is unknown).
    pub proton_needs_nvapi_env: bool,
    /// Where the NVIDIA driver's Wine NGX DLLs were found, if anywhere.
    pub nvngx_wine_dir: Option<std::path::PathBuf>,
    /// nvngx.dll present inside the game's Proton prefix (None = no prefix known).
    pub prefix_nvngx: Option<bool>,
    pub driver_version: Option<String>,
    /// Feeder path without a native d3dcompiler_47.dll next to the exe.
    pub d3dcompiler_missing_feeder: bool,
    pub steam_running: bool,
}

/// Findings about the host setup (launch options, driver, Proton) — pure and
/// fixture-testable; `ctx` carries every fact.
pub fn host_findings(st: &GameStatus, ctx: &HostContext) -> Vec<Finding> {
    let mut out = Vec::new();
    if !ctx.relevant {
        return out;
    }
    match ctx.launcher {
        Some("Steam") => {
            if ctx.steam_options.is_empty() {
                out.push(warn(
                    "No Steam user config (localconfig.vdf) found; cannot verify launch options.",
                ));
            }
            for (label, satisfied) in &ctx.steam_options {
                if *satisfied {
                    out.push(ok(format!("Steam launch options set ({label}).")));
                } else {
                    out.push(bad(format!(
                        "Steam launch options incomplete ({label}): without them Proton loads its \
                         own dxgi and nothing injects. Run --launch-options, or paste into \
                         Properties -> Launch Options: {}",
                        ctx.required_display
                    )));
                }
            }
            match &ctx.proton {
                Some(p) => {
                    if ctx.proton_needs_nvapi_env {
                        out.push(warn(format!(
                            "Proton \"{p}\" predates default-on NVAPI; PROTON_ENABLE_NVAPI=1 is \
                             required (a Proton 9+ build is a better fix)."
                        )));
                    } else {
                        out.push(ok(format!("Proton: {p}.")));
                    }
                }
                None => out.push(warn(
                    "No Proton mapping found for this game; Steam's default applies.",
                )),
            }
            if ctx.steam_running {
                out.push(warn(
                    "Steam is running — launch-option changes need it closed.",
                ));
            }
        }
        Some(other) => out.push(warn(format!(
            "{other} game: verify WINEDLLOVERRIDES is set in {other}'s per-game environment \
             settings ({}).",
            ctx.required_display
        ))),
        None => out.push(warn(format!(
            "This folder maps to no known launcher; wherever it runs under Proton/Wine it needs: {}",
            ctx.required_display
        ))),
    }
    match &ctx.nvngx_wine_dir {
        Some(dir) => out.push(ok(format!(
            "NVIDIA NGX Wine DLLs present ({}).",
            dir.display()
        ))),
        None => out.push(bad(
            "The NVIDIA driver's Wine NGX DLLs (nvngx.dll/_nvngx.dll under /usr/lib/nvidia/wine \
             or the distro equivalent) were not found — DLSS cannot initialise under Proton. \
             Install the driver's Wine/NGX component (e.g. nvidia-utils or libnvidia-ngx).",
        )),
    }
    if let Some(present) = ctx.prefix_nvngx {
        if present {
            out.push(ok("nvngx.dll present in the game's Proton prefix."));
        } else {
            out.push(warn(
                "nvngx.dll not yet in the game's Proton prefix; Proton copies it on the first \
                 launch with NVAPI enabled — start the game once, then re-run --diagnose.",
            ));
        }
    }
    match &ctx.driver_version {
        Some(v) => out.push(ok(format!("NVIDIA kernel driver {v}."))),
        None => out.push(warn(
            "NVIDIA kernel module not loaded (no /sys/module/nvidia/version).",
        )),
    }
    if ctx.d3dcompiler_missing_feeder && st.mode == game::Mode::Feeder {
        out.push(warn(
            "No d3dcompiler_47.dll next to the game exe; ReShade falls back to Proton's builtin \
             compiler, which usually works. If effects fail to compile in ReShade.log, copy a \
             native d3dcompiler_47.dll next to the exe and add d3dcompiler_47=n to \
             WINEDLLOVERRIDES.",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::testutil::*;

    fn setup(feeder: bool) -> (tempfile::TempDir, std::path::PathBuf) {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        if !feeder {
            fs::write(t.path().join(game::DLSS_DLL), b"x").unwrap();
        }
        (t, exe)
    }

    fn host_ctx() -> HostContext {
        HostContext {
            relevant: true,
            launcher: Some("Steam"),
            steam_options: vec![("user 111".into(), true)],
            required_display: "WINEDLLOVERRIDES=\"dxgi=n,b\" %command%".into(),
            proton: Some("GE-Proton11-5".into()),
            proton_needs_nvapi_env: false,
            nvngx_wine_dir: Some(std::path::PathBuf::from("/usr/lib/nvidia/wine")),
            prefix_nvngx: Some(true),
            driver_version: Some("610.57.04".into()),
            d3dcompiler_missing_feeder: false,
            steam_running: false,
        }
    }

    #[test]
    fn host_findings_all_green() {
        let (_t, exe) = setup(true);
        let st = game::inspect(&exe).unwrap();
        let f = host_findings(&st, &host_ctx());
        assert!(!f.is_empty());
        assert!(f.iter().all(|x| x.level == Level::Ok), "{f:?}");
    }

    #[test]
    fn host_findings_missing_launch_options_is_bad() {
        let (_t, exe) = setup(true);
        let st = game::inspect(&exe).unwrap();
        let mut ctx = host_ctx();
        ctx.steam_options = vec![("user 111".into(), false)];
        let f = host_findings(&st, &ctx);
        let bad = f.iter().find(|x| x.level == Level::Bad).unwrap();
        assert!(bad.text.contains("launch options incomplete"));
        assert!(bad.text.contains("WINEDLLOVERRIDES"));
    }

    #[test]
    fn host_findings_old_proton_and_missing_ngx() {
        let (_t, exe) = setup(true);
        let st = game::inspect(&exe).unwrap();
        let mut ctx = host_ctx();
        ctx.proton = Some("proton_63".into());
        ctx.proton_needs_nvapi_env = true;
        ctx.nvngx_wine_dir = None;
        let f = host_findings(&st, &ctx);
        assert!(f
            .iter()
            .any(|x| x.level == Level::Warn && x.text.contains("predates default-on NVAPI")));
        assert!(f
            .iter()
            .any(|x| x.level == Level::Bad && x.text.contains("Wine NGX DLLs")));
    }

    #[test]
    fn host_findings_d3dcompiler_warns_only_on_feeder() {
        let mut ctx = host_ctx();
        ctx.d3dcompiler_missing_feeder = true;
        let (_t, exe) = setup(true); // feeder mode
        let st = game::inspect(&exe).unwrap();
        assert!(host_findings(&st, &ctx)
            .iter()
            .any(|x| x.text.contains("d3dcompiler_47")));
        let (_t2, exe2) = setup(false); // native mode
        let st2 = game::inspect(&exe2).unwrap();
        assert!(!host_findings(&st2, &ctx)
            .iter()
            .any(|x| x.text.contains("d3dcompiler_47")));
    }

    #[test]
    fn host_findings_silent_when_irrelevant() {
        let (_t, exe) = setup(true);
        let st = game::inspect(&exe).unwrap();
        assert!(host_findings(&st, &HostContext::default()).is_empty());
    }

    #[test]
    fn no_reshade_log_is_fatal() {
        let (_t, exe) = setup(true);
        let f = run(&exe).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, Level::Bad);
        assert!(f[0].text.contains("never loaded"));
    }

    #[test]
    fn native_without_game_dlss_call() {
        let (t, exe) = setup(false);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f
            .iter()
            .any(|x| x.text.contains("game's own DLSS never ran")));
        assert!(f
            .iter()
            .any(|x| x.level == Level::Ok && x.text.contains("add-on registered")));
    }

    #[test]
    fn addon_load_failure_is_explained() {
        let (t, exe) = setup(false);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nFailed to load add-on from 'C:\\g\\renodx-dlss5.addon64' with error code 2148073478!\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f.iter().any(|x| x.text.contains("unsigned DLLs")));
    }

    #[test]
    fn feeder_provider_not_enabled() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] effects: DLSS5_Feed.fx technique found, DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> none (not installed)\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f
            .iter()
            .any(|x| x.text.contains("motion-vector provider is not enabled")));
    }

    #[test]
    fn healthy_session_says_so() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\ninline feature 18 evaluation succeeded (count=60)\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] feature ready: 3840x2160 DLAA\n[feed] frame 1 delivered\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f.iter().all(|x| x.level == Level::Ok), "{f:?}");
        assert!(f.iter().any(|x| x.text.contains("raise NR Intensity")));
    }
}
