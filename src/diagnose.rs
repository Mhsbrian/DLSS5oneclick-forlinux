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

/// `NVSDK_NGX_*_Init -> 0xBAD00001` in a log: NGX itself refused. Add what the
/// system says about NGX Core, which is the usual cause on capable hardware.
/// `exe` is the game (and, for a 32-bit game, its helper) whose Windows GPU
/// preference is worth naming: on a hybrid machine a process started on the
/// iGPU gets exactly this error, because NGX does not exist there (#25).
fn ngx_init_failure_for(log: &str, exe: Option<&std::path::Path>, out: &mut Vec<Finding>) {
    let Some(line) = log
        .lines()
        .find(|l| l.contains("NVSDK_NGX") && l.contains("Init") && l.contains("0xBAD00001"))
    else {
        return;
    };
    if crate::gpupref::hybrid() {
        let names = crate::gpupref::real_adapters();
        #[cfg(target_os = "linux")]
        {
            let _ = exe;
            out.push(bad(format!(
                "More than one GPU is present ({}). If the game (under Proton) started on the \
                 integrated GPU, NGX does not exist there and every Init answers 0xBAD00001. \
                 Force the NVIDIA card by adding these to the game's launch options: \
                 __NV_PRIME_RENDER_OFFLOAD=1 __GLX_VENDOR_LIBRARY_NAME=nvidia \
                 (with DXVK, also DRI_PRIME=1). On a desktop where NVIDIA already drives the \
                 display this is not the cause.",
                names.join(", ")
            )));
        }
        #[cfg(not(target_os = "linux"))]
        {
            let set = exe.is_some_and(|e| {
                crate::gpupref::get(e).is_some_and(|v| crate::gpupref::is_high_performance(&v))
            });
            out.push(bad(format!(
                "More than one GPU vendor on this machine ({}), and Windows decides which one              a process starts on. Started on the integrated GPU, NGX does not exist and              every Init answers 0xBAD00001 — this is the most likely cause here{}.              Settings ▸ System ▸ Display ▸ Graphics ▸ Add a desktop app ▸ pick the game's exe              (and, for a 32-bit game, host64\\dlss5-feed-host64.exe) ▸ Options ▸ High performance.              Install sets that for you from this version on.",
                names.join(", "),
                if set {
                    ", though the preference is already set to high performance for that exe"
                } else {
                    ""
                }
            )));
        }
    }
    let system = crate::ngx::describe();
    // Reported on three machines (RTX 4070, 5080, 5090) with NGX Core present and
    // driver 616.56, always on the Feeder's own in-process D3D12 device. The same
    // chain initialises NGX fine in the 32-bit host64 helper (a separate process)
    // and on the native path where the game owns the device, so the installed
    // files are not what decides it.
    let advice = if crate::ngx::healthy() {
        "Your NGX runtime and driver are fine, so this is NGX refusing the Feeder's private          D3D12 device inside the game process, which has been reported on several machines.          Worth doing: install into a game that ships its own DLSS (that path opens no private          device) to confirm NGX works for you, then report this log at          github.com/jlrouzies-fr/DLSS5-Feeder, where that device is created."
    } else {
        "Fix that first, then run Install again: reinstall the NVIDIA driver with a Custom          install that keeps every component (616.56 or newer)."
    };
    out.push(bad(format!(
        "NGX refused to initialise: {}. 0xBAD00001 is FeatureNotSupported, which NGX also          answers when its runtime is not on the system — not a ReShade, shader or add-on          problem. {system}. {advice}",
        line.trim()
    )));
}

/// Newest Feeder known at build time; only used to nudge users off stale copies.
const CURRENT_FEEDER: &str = "0.12.0";

fn version_key(v: &str) -> Vec<u64> {
    v.split(['.', '-'])
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
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

/// The exe ReShade actually loaded into, from its first line:
/// `... loaded from '...dxgi.dll' into 'C:\\...bg3_dx11.exe' (0x...)`.
fn reshade_host_exe(log: &str) -> Option<String> {
    let line = log
        .lines()
        .find(|l| l.contains("loaded from") && l.contains(" into "))?;
    let path = line.split(" into ").nth(1)?.split('\'').nth(1)?;
    // The log always holds a Windows path (`C:\...\bg3_dx11.exe`), so split on
    // both separators — `Path::file_name` treats `\` as an ordinary character
    // when this tool runs on Linux and would return the whole path.
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Findings for a game folder, in reading order.
pub fn diagnose(st: &GameStatus) -> Vec<Finding> {
    let d = st.game_dir();
    let mut out = Vec::new();

    // ── which neural model is installed ────────────────────
    // Two builds of nvngx_dlssnr.dll are in circulation and only the version
    // resource separates them; every failing RTX 50 report so far carries the
    // .SF one, so the log has to name it.
    let consumer = st.consumer_dir();
    for p in [d.join(game::DLSSNR_DLL), consumer.join(game::DLSSNR_DLL)] {
        if !p.is_file() {
            continue;
        }
        if let Some(v) = crate::ngx::file_version(&p) {
            out.push(ok(format!(
                "DLSS 5 model {}: {v} — {}",
                if p.parent() == Some(d) {
                    "beside the exe"
                } else {
                    "in host64"
                },
                crate::ngx::model_build(&v)
            )));
        }
        break;
    }

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
        // The add-on hooks NVSDK_NGX_D3D12_*. A game whose DLSS runs on D3D11
        // calls the D3D11 entry points, which it never sees, so "no create"
        // is expected until the bridge is installed (#33, BG3 DX11).
        if st.api == game::Api::Dx11 && !st.bridge {
            out.push(bad(
                "No NGX call was intercepted, and this is a Direct3D 11 game with its own \
                 DLSS: the add-on hooks the D3D12 NGX entry points, but the game calls the \
                 D3D11 ones, so it can never see them. The DX11 bridge covers exactly this \
                 and is not installed here — run Install on this exe.",
            ));
        } else {
            out.push(bad(
                "No NGX call was intercepted: this game's own DLSS never ran. Turn DLSS on \
                 in the game's graphics settings (the add-on hooks the game's DLSS calls; \
                 without them it has nothing to work with).",
            ));
        }
    }

    // A game with more than one executable (a Vulkan build and a DX11 build,
    // a launcher and the game) can be installed for one and played through
    // another: ReShade loads, everything looks right, nothing is hooked (#33).
    if let Some(loaded) = reshade_host_exe(&rs) {
        let ours = st
            .exe
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase());
        if ours.is_some_and(|o| o != loaded.to_ascii_lowercase()) {
            out.push(warn(format!(
                "ReShade loaded into {loaded}, but this install was set up for {}. Those are \
                 different executables, and the install is tuned to the one you picked (the \
                 DX11 bridge in particular). Point the tool at {loaded} and run Install again.",
                st.exe.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
    }

    // ── DX11 bridge (native DLSS on D3D11) ──────────────────────────
    if let Some(bl) = read(d, "dlss5-bridge.log") {
        if bl.contains("### CRASH RECORDED ###") {
            let exc = bl
                .lines()
                .find(|l| l.contains("exception 0x"))
                .map(str::trim)
                .unwrap_or("see the log");
            out.push(bad(format!(
                "The DX11 bridge recorded a crash during its work ({exc}) — its \"game renders                  normally\" stop message notwithstanding, an exception like this can take the                  game down with it. To play now: set stage=0 in dlss5-bridge.cfg (bridge off, no                  neural rendering) or Remove. Please attach dlss5-bridge.log to an issue at                  github.com/NIGos/dlss5-bridge — the crash block in it is exactly what its                  author asks for. The OptiScaler engine is an alternative path that needs no                  bridge."
            )));
        } else if let Some(line) = bl.lines().rev().find(|l| l.contains("stopped:")) {
            out.push(bad(format!("The DX11 bridge stopped: {}", line.trim())));
        }
        if let Some(line) = bl
            .lines()
            .find(|l| l.contains("D3D12CreateDevice failed 0x887E0003"))
        {
            out.push(bad(format!(
                "{} — 0x887E0003 is D3D12_ERROR_INVALID_REDIST: this game ships a DirectX 12 \
                 Agility SDK (a D3D12\\D3D12Core.dll beside the exe) and every D3D12 device in \
                 the process must match it, which the bridge's private device cannot. Not \
                 something this tool sets. Worth trying: rename the game's D3D12 folder so it \
                 falls back to the Windows runtime, and report the log to \
                 github.com/NIGos/dlss5-bridge.",
                line.trim()
            )));
        } else if bl.contains("session failed") {
            out.push(bad(
                "The DX11 bridge could not open its private D3D12 session. Under Proton that                  device runs on vkd3d-proton, which the bridge supports since 1.4.6 — re-run                  Install (it refreshes the bridge to the current build) and use a recent Proton;                  if it persists, attach dlss5-bridge.log to an issue at                  github.com/NIGos/dlss5-bridge.",
            ));
        } else if bl.contains("frames:") {
            out.push(ok(
                "The DX11 bridge opened its D3D12 session and is delivering frames.",
            ));
        }
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
        // The first effects line of a session always says "none": effects are
        // not compiled yet. Only the last one describes the running state (#6).
        let last_effects = fd
            .lines()
            .rev()
            .find(|l| l.contains("[feed] effects:"))
            .unwrap_or("");
        if last_effects.contains("-> none (not installed)") {
            out.push(bad(
                "The motion-vector provider is not enabled. In ReShade's Home tab enable \
                 \"LUMENITE: Kernel 2.0\" ABOVE \"DLSS5_Feed\", then reload effects.",
            ));
        }
        ngx_init_failure_for(&fd, Some(&st.exe), &mut out);
        // 32-bit games: the work happens in host64\, and its own log names the reason.
        if let Some(hl) = read(&d.join(game::HOST_DIR), "dlss5-feed-host.log") {
            if hl.contains("feature ready") {
                out.push(ok(
                    "The host64 helper built its DLSS feature (feature ready … DLAA).",
                ));
            }
            ngx_init_failure_for(&hl, Some(&st.consumer_dir().join(game::HOST_EXE)), &mut out);
        } else if st.is32() {
            out.push(warn(
                "No host64\\dlss5-feed-host.log yet: the 64-bit helper has not started. It is \
                 spawned by the first fed frame, so enable Lumenite_Kernel + DLSS5_Feed in \
                 ReShade's Home tab and play a moment first.",
            ));
        }
        if let Some(ver) = fd
            .lines()
            .next()
            .and_then(|l| {
                // "HH:MM:SS.mmm  dlss5-feed 0.12.0 (built ...) attached."
                let mut it = l.split_whitespace();
                it.find(|t| t.starts_with("dlss5-feed"))?;
                it.next()
            })
            .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            if version_key(ver) < version_key(CURRENT_FEEDER) {
                out.push(warn(format!(
                    "DLSS5-Feeder {ver} in the log is older than {CURRENT_FEEDER}; re-run Install \
                     to refresh it (since 0.9.1 an existing Feeder is updated)."
                )));
            }
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
    /// The RTX 40 MFG unlock is installed in this game.
    pub mfg_installed: bool,
    /// Last meaningful line of the MFG unlock's own log in the Proton prefix,
    /// when it wrote one (proof its ASI + core loaded under Proton); `None`
    /// means installed but no log found yet.
    pub mfg_log_tail: Option<String>,
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
    // Only the positive confirmation lives here; the "missing bridge" case is
    // owned by diagnose() proper (it explains, in the NGX-interception
    // narrative, why the D3D11 game's calls are never seen), so this does not
    // double-report it.
    if st.mode == game::Mode::Native && st.api == game::Api::Dx11 && st.bridge {
        out.push(ok(
            "DX11 bridge installed (this game's own DLSS is D3D11; the bridge mirrors it onto a \
             private D3D12 device — vkd3d-proton under Proton, supported by bridge 1.4.6+; \
             Install refreshes the bridge when a newer build is published).",
        ));
    }
    if ctx.d3dcompiler_missing_feeder && st.mode == game::Mode::Feeder {
        out.push(warn(
            "No d3dcompiler_47.dll next to the game exe; ReShade falls back to Proton's builtin \
             compiler, which usually works. If effects fail to compile in ReShade.log, copy a \
             native d3dcompiler_47.dll next to the exe and add d3dcompiler_47=n to \
             WINEDLLOVERRIDES.",
        ));
    }
    if ctx.mfg_installed {
        match &ctx.mfg_log_tail {
            Some(tail) => out.push(ok(format!(
                "RTX 40 MFG unlock loaded under Proton (its ASI wrote a log). Last line: {tail}. \
                 Set the multiplier in ReShade → DLSS MFG; if it will not go above 1X the mod \
                 has failed closed on this Streamline wrapper — report the log to \
                 github.com/dashdogy/RTX40MFG-Unlock."
            ))),
            None => out.push(warn(
                "RTX 40 MFG unlock is installed but its ASI has written no log yet — it may not \
                 have attached. Confirm the game imports the proxy DLL and that its \
                 WINEDLLOVERRIDE is in the launch options (--launch-options), then play once.",
            )),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// Baldur's Gate 3 ships bg3.exe (Vulkan) and bg3_dx11.exe; installing for
    /// one and playing the other leaves everything looking right and nothing
    /// hooked (#33). The exe name is in ReShade's first line.
    #[test]
    fn reshade_host_exe_reads_the_first_line() {
        let log = "01:30:11:810 [17792] | INFO  | Initializing crosire's ReShade version '6.8.0.2155' (64-bit) loaded from 'C:\\\\Program Files (x86)\\\\Steam\\\\steamapps\\\\common\\\\Baldurs Gate 3\\\\bin\\\\dxgi.dll' into 'C:\\\\Program Files (x86)\\\\Steam\\\\steamapps\\\\common\\\\Baldurs Gate 3\\\\bin\\\\bg3_dx11.exe' (0x64317982) ...";
        assert_eq!(
            super::reshade_host_exe(log).as_deref(),
            Some("bg3_dx11.exe")
        );
        assert!(super::reshade_host_exe("nothing useful here").is_none());
    }

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
            mfg_installed: false,
            mfg_log_tail: None,
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
    fn bridge_recorded_crash_is_named() {
        let (t, exe) = setup(false);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\nRegistered add-on \"DLSS 5 Neural Rendering\"\ninline feature 18 evaluation succeeded\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-bridge.log"),
            "stopped: NGX raised an exception inside the D3D12 evaluate. The game renders normally.\n### CRASH RECORDED ###\n  exception 0xE06D7363 at 00006FFFFFBFD947\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        let bad = f
            .iter()
            .find(|x| x.level == Level::Bad && x.text.contains("recorded a crash"))
            .expect("crash finding");
        assert!(bad.text.contains("0xE06D7363"));
        assert!(bad.text.contains("stage=0"));
        // The misleading "stopped" line is superseded, not duplicated.
        assert!(!f.iter().any(|x| x.text.starts_with("The DX11 bridge stopped:")));
    }

    #[test]
    fn bridge_session_failure_is_bad() {
        let (t, exe) = setup(false);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\nRegistered add-on \"DLSS 5 Neural Rendering\"\ninline feature 18 evaluation succeeded\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-bridge.log"),
            "bridge 1.4.5\nsession failed: D3D12CreateDevice returned E_NOINTERFACE\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        let bad = f
            .iter()
            .find(|x| x.level == Level::Bad && x.text.contains("private D3D12 session"))
            .expect("session failure finding");
        assert!(bad.text.contains("vkd3d-proton"));
    }

    /// The missing-bridge case is owned by diagnose() (needs a ReShade.log to
    /// establish "no NGX call"); host_findings only confirms an installed one.
    #[test]
    fn native_dx11_missing_bridge_is_caught_by_diagnose() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = game::testutil::make_pe_with_imports(
            &t.path().join("game.exe"),
            &["d3d11.dll"],
            2_000_000,
        );
        fs::write(t.path().join(game::DLSS_DLL), b"x").unwrap(); // native mode
        // ReShade loaded and the add-on registered, but nothing was hooked.
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        let st = game::inspect(&exe).unwrap();
        assert_eq!(st.api, game::Api::Dx11);
        assert_eq!(st.mode, game::Mode::Native);
        // diagnose() (no bridge) names the bridge; host_findings does not repeat it.
        let d = diagnose(&st);
        assert!(d
            .iter()
            .any(|x| x.level == Level::Bad && x.text.contains("DX11 bridge covers")));
        let h = host_findings(&st, &host_ctx());
        assert!(!h.iter().any(|x| x.text.contains("bridge")));
        // With the bridge present, host_findings confirms it and diagnose stops warning.
        fs::write(t.path().join(game::BRIDGE_ADDON), b"x").unwrap();
        let st = game::inspect(&exe).unwrap();
        assert!(host_findings(&st, &host_ctx())
            .iter()
            .any(|x| x.level == Level::Ok && x.text.contains("DX11 bridge installed")));
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
    fn feeder_last_effects_line_wins_and_ngx_init_failure_named() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "12:33:33.151  dlss5-feed 0.7.0 (built Aug 31 2026) attached.\n\
             [feed] effects: DLSS5_Feed.fx technique MISSING, DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> none (not installed)\n\
             [feed] effects: DLSS5_Feed.fx technique found, DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> Lumenite_Kernel (enabled)\n\
             [feed] NVSDK_NGX_D3D12_Init -> 0xBAD00001 (FeatureNotSupported)\n\
             stopped: the D3D12/NGX session failed to start.\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(!f
            .iter()
            .any(|x| x.text.contains("motion-vector provider is not enabled")));
        assert!(f
            .iter()
            .any(|x| x.text.contains("NGX refused to initialise")));
        assert!(f
            .iter()
            .any(|x| x.text.contains("0.7.0 in the log is older")));
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
