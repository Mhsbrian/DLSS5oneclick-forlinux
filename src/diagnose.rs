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
const CURRENT_FEEDER: &str = "0.15.1";

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
    fs::read_to_string(game::join_ci(dir, &[name])).ok()
}

/// The resolution the NR model was last running at, from OptiScaler.log's
/// "DLSS-NR running at WxH" line — the size that decides the GPU load.
fn last_nr_resolution(log: &str) -> String {
    log.rmatch_indices("running at ")
        .next()
        .and_then(|(i, m)| log[i + m.len()..].split_whitespace().next())
        .map(|s| s.trim_end_matches(',').to_string())
        .unwrap_or_else(|| "full resolution".to_string())
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
    // A Linux build only ever looks at games running under Proton. A Windows
    // build can still be reading a Wine session's logs, which give it away.
    diagnose_with(st, cfg!(target_os = "linux"))
}

/// `diagnose`, told whether the game runs under Proton/Wine: the switch for
/// advice that is right on Windows and wrong there. Tests pick either side.
fn diagnose_with(st: &GameStatus, proton: bool) -> Vec<Finding> {
    let d = st.game_dir();
    let mut out = Vec::new();
    let consumer = st.consumer_dir();
    let rs_log = read(&consumer, "ReShade.log").or_else(|| read(&consumer, "ReShade2.log"));
    // Wine and Proton substitute their own d3dcompiler_47.dll, whose HLSL
    // compiler is vkd3d-shader. It does not implement every attribute ReShade
    // emits, and says so in its own words (#70).
    let wine_hlsl = rs_log
        .as_deref()
        .is_some_and(|l| l.contains("not yet implemented feature"));
    let wine = proton || wine_hlsl;

    // ── a game-shipped HLSL compiler shadowing the system one ──────
    // The add-on compiles its NR pass at cs_5_1. A d3dcompiler_47.dll that
    // ships with the game is loaded in preference to System32's, and an old
    // one does not know that target: "error X3506: unrecognized compiler
    // target" and no neural rendering, with everything else looking correct.
    // Not under Proton: there the game's own copy is Microsoft's compiler, the
    // one that works, standing in for Wine's builtin one that does not.
    let compiler = game::join_ci(d, &["d3dcompiler_47.dll"]);
    if compiler.is_file() && !wine {
        let ver = crate::ngx::file_version(&compiler).unwrap_or_else(|| "unknown".into());
        out.push(warn(format!(
            "The game ships its own d3dcompiler_47.dll ({ver}), which Windows loads instead of \
             System32's. If it predates shader model 5.1 the DLSS 5 pass cannot compile \
             (error X3506). Rename it to d3dcompiler_47.dll.bak and start the game again; \
             almost every game runs fine on the system copy. On Wine/Proton, leave it: a \
             copy of Microsoft's compiler beside the game, loaded through a WINEDLLOVERRIDES \
             entry, is what got DLSS 5 compiling there at all (#76)."
        )));
    }

    // ── dgVoodoo's reported VRAM vs what the game was told ─────────
    // A game that reads VRAM from the adapter and sizes its own pools from it
    // has to be told the same number, or it sizes for one figure and allocates
    // against another. GTA IV is the case in hand: with dgVoodoo reporting
    // 4096 MB and no matching -availablevidmem it black-screens after load,
    // and with no conf at all it takes dgVoodoo's stock 256 MB and dies with
    // "TEXP60: Unable to create color render target" (#69).
    let conf = game::join_ci(d, &["dgVoodoo.conf"]);
    if conf.is_file() {
        let vram = fs::read_to_string(&conf).ok().and_then(|t| {
            t.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("VRAM"))
                .and_then(|(_, v)| v.trim().trim_end_matches("MB").trim().parse::<u32>().ok())
        });
        let cmdline = game::join_ci(d, &["commandline.txt"]);
        if let (Some(vram), true) = (vram, cmdline.is_file()) {
            let text = fs::read_to_string(&cmdline).unwrap_or_default();
            if !text.to_ascii_lowercase().contains("-availablevidmem") {
                out.push(warn(format!(
                    "dgVoodoo reports {vram} MB of video memory and this game reads that number \
                     to size its own memory pools, but commandline.txt does not set \
                     -availablevidmem. Add \"-availablevidmem {}\" to commandline.txt — slightly \
                     below the dgVoodoo figure on purpose, which is what the dgVoodoo guides for \
                     this engine call for. Without it the game sizes for one number and allocates \
                     against another, which shows up as a black screen after loading or as \
                     TEXP60 / TEXP70 at startup.",
                    vram.saturating_sub(64).max(256)
                )));
            }
        }
    }

    // ── which neural model is installed ────────────────────
    // Two builds of nvngx_dlssnr.dll are in circulation and only the version
    // resource separates them; every failing RTX 50 report so far carries the
    // .SF one, so the log has to name it.
    for p in [
        game::join_ci(d, &[game::DLSSNR_DLL]),
        game::join_ci(&consumer, &[game::DLSSNR_DLL]),
    ] {
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

    // ── OptiScaler engine (its own log, no ReShade) ─────────────────
    // The OptiScaler engine does not use ReShade, so ReShade.log never exists;
    // everything is in OptiScaler.log. Reading it here also catches the GPU
    // device-lost fault the NR runtime drives under Proton, which is otherwise
    // invisible.
    if st.opti {
        let Some(ol) = read(d, "OptiScaler.log") else {
            out.push(bad(
                "No OptiScaler.log next to the game exe: OptiScaler never loaded. Either the game \
                 was not started since the install, or it does not load dxgi.dll (wrong exe picked, \
                 or a launcher starts a different one). Check the exe with --check.",
            ));
            return out;
        };
        if ol.contains("VK_ERROR_DEVICE_LOST") || ol.contains("GPU Crash") {
            out.push(bad(format!(
                "The GPU crashed while DLSS 5 was running (VK_ERROR_DEVICE_LOST in OptiScaler.log; \
                 look for an Xid fault in `journalctl -k`). The neural runtime drives the GPU into \
                 a device-lost fault under Proton, worst at full resolution — the model was running \
                 at {}. Cheapest first: lower the Model resolution (the 50% button in this tool, or \
                 --model-res=50 — the model then runs at about a quarter of the pixels, a large drop \
                 in GPU load, often enough to stop the fault); lower the game's output resolution; \
                 try a different Proton; or Remove if it keeps faulting.",
                last_nr_resolution(&ol)
            )));
        } else if ol.contains("DlssNr") && (ol.contains("composition") || ol.contains("Dispatch")) {
            out.push(ok(
                "OptiScaler DLSS 5 neural rendering is running (composition dispatches in OptiScaler.log).",
            ));
        } else if ol.contains("DlssNr") {
            out.push(warn(
                "OptiScaler loaded and DLSS-NR initialised, but no composition ran yet — press \
                 Insert to open the overlay, make sure Neural Rendering is on, and move the camera.",
            ));
        } else {
            out.push(warn(
                "OptiScaler loaded but DLSS-NR has not run — press Insert and enable Neural Rendering.",
            ));
        }
        return out;
    }

    // ── ReShade side ────────────────────────────────────────────────
    // The DLSS 5 add-on runs under the ReShade in `consumer_dir()`: beside the
    // exe for a 64-bit game, in host64\ for a 32-bit one. Reading the game
    // folder's log for a 32-bit game reads the *feeder's* 32-bit ReShade, which
    // never loads the add-on, so every 32-bit report came back "the add-on
    // never registered" no matter how healthy the install was (#69).
    // A 32-bit game has two ReShades: the one beside the exe, which the Home key
    // opens and which loads the feeder, and the 64-bit one in host64\ that hosts
    // the neural add-on. Reporting only the second leaves "Home does nothing"
    // unexplained, which is the first thing the player actually notices (#69).
    if st.is32() && !game::is_reshade_dll(&game::join_ci(d, &[game::RESHADE_PROXY])) {
        out.push(bad(format!(
            "No ReShade beside the game exe: {} is missing or is not ReShade, so the Home key \
             opens nothing and the feeder never loads. That is upstream of anything in host64\\. \
             Run Remove and then Install again, and if it comes back missing, check antivirus \
             history for {}.",
            game::RESHADE_PROXY,
            game::RESHADE_PROXY
        )));
    }
    let Some(rs) = rs_log else {
        out.push(bad(if st.is32() {
            "No host64\\ReShade.log: the 64-bit helper's ReShade never loaded, which is what \
             \"host lost: pipe never appeared\" in dlss5-feed.log means. Look in \
             host64\\dlss5-feed-host.log for the reason, and check antivirus did not remove \
             anything from host64\\."
        } else {
            "No ReShade.log next to the game exe: ReShade never loaded. Either the game was not \
             started since the install, or it does not load dxgi.dll (wrong exe picked, or a \
             launcher starts a different one). Check the exe with --check."
        }));
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
        out.push(bad(format!(
            "The DLSS 5 add-on never registered. renodx-dlss5.addon64 is missing from {}, \
             disabled in ReShade's Add-ons tab, or quarantined by antivirus.",
            if st.is32() {
                "host64\\ (where a 32-bit game's add-on lives)"
            } else {
                "the game folder"
            }
        )));
    }
    if rs.contains("NR toggled ON") && !rs.contains("NR toggled OFF") {
        out.push(ok("Neural rendering was toggled ON (F6)."));
    } else if rs.contains("NR toggled OFF") {
        out.push(warn(
            "The log's last F6 state may be OFF — press F6 in game and watch the add-on's panel.",
        ));
    }
    // A failed evaluate takes priority over an earlier success: a title that
    // rejects the upscaling evaluate but not the native one logs both, and the
    // rejection is the one the player is looking at (a black frame).
    if let Some(line) = rs.lines().find(|l| l.contains("feature 18 evaluate failed")) {
        // The NR model was created and evaluated, but the runtime rejected the
        // call. 0xbad00005 is NVSDK_NGX_Result_FAIL_InvalidParameter, and it
        // shows up on the *upscaling* path under Proton -- vkd3d-proton's D3D12
        // does not satisfy the signed NR runtime the way the native driver does
        // -- while native-resolution (DLAA) frames evaluate fine. The rejected
        // frame comes out black, so DLSS upscaling flickers black on this title.
        let proton = line.contains("0xbad00005") || line.contains("0xBAD00005");
        let dlaa_works = rs.contains("inline feature 18 evaluation succeeded");
        out.push(bad(format!(
            "{} — neural rendering compiled and ran, but the DLSS 5 runtime rejected the \
             evaluate.{}{} This is the signed NR runtime under Proton, not the tool's setup.",
            line.trim(),
            if proton {
                " 0xbad00005 is InvalidParameter, and it appears on the upscaling path under \
                 Proton (vkd3d-proton), leaving that frame black — the flicker you see."
            } else {
                ""
            },
            if dlaa_works {
                " Native-resolution frames did evaluate here, so set the game's DLSS to DLAA \
                 (no upscaling) for working neural rendering, or press F6 to turn it off and \
                 keep the game's own DLSS."
            } else {
                " Set the game's DLSS to DLAA (native resolution, no upscaling), or press F6 to \
                 turn neural rendering off and keep the game's own DLSS."
            }
        )));
    } else if rs.contains("inline feature 18 evaluation succeeded") {
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
        if st.feeder {
            out.push(warn(
                "Mode is Native DLSS (game ships its own DLSS), but dlss5-feed.addon64 is still \
                 present. Feeder Optimize does not apply here — NR is game/renodx. Run Remove \
                 (incl. Feeder leftovers) or Install again so Native cleanup drops the Feeder.",
            ));
        }
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

    // Linux/Proton: ReShade generates HLSL with [fastopt] for shader model 4
    // and up, and Wine's d3dcompiler_47 (vkd3d-shader) has not implemented it,
    // so DLSS5_Feed.fx and the Lumenite shaders never build — with the feed
    // add-on then reporting its technique missing, which reads like our bug
    // rather than a missing compiler (#70).
    // The add-on's neural pass compiles through the same compiler. When that
    // failed as well it is the same cause with the same fix, so one finding.
    let nr_pass_failed = rs
        .lines()
        .find(|l| l.contains("proxy encode compilation failed") || l.contains("is not defined"))
        .map(str::trim);
    if wine_hlsl {
        let line = rs
            .lines()
            .find(|l| l.contains("not yet implemented feature"))
            .unwrap_or("")
            .trim();
        let mut t = format!(
            "The effects failed to compile in Wine/Proton's own HLSL compiler: {line} \
             That message comes from vkd3d-shader, which Wine's d3dcompiler_47.dll uses; \
             ReShade emits attributes it has not implemented. What a reporter measured working \
             (#76, Proton 10.0-4, Elden Ring, no launch options at all): put a copy of \
             Microsoft's real d3dcompiler_47.dll in the game folder, beside the executable. \
             The feed then reports it as \"not System32, but it accepts cs_5_1 -- fine\" and \
             the effects compile. If the prefix still loads Wine's copy instead, add \
             WINEDLLOVERRIDES=\"d3dcompiler_47=n\" %command% to the launch options; \
             protontricks <appid> d3dcompiler_47 (or winetricks d3dcompiler_47) puts the real \
             one in the prefix if you do not have a copy to hand (Install does this itself for \
             a Steam game when either is installed)."
        );
        if let Some(nr) = nr_pass_failed {
            t.push_str(&format!(
                " The add-on's neural-rendering pass failed in the same compiler ({nr}), so \
                 the same step fixes it."
            ));
        }
        out.push(bad(t));
    }

    // The compile failure itself, which is unambiguous when it appears.
    if let Some(line) = rs
        .lines()
        .find(|l| l.contains("X3506") || l.contains("unrecognized compiler target"))
    {
        out.push(bad(format!(
            "{} — the HLSL compiler in this process is too old for the DLSS 5 pass. That is \
             a d3dcompiler_47.dll shipped with the game, loaded in preference to System32's. \
             Rename it (d3dcompiler_47.dll.bak) and start the game again.{}",
            line.trim(),
            if wine {
                " Under Proton, first make sure Microsoft's real d3dcompiler_47 is in the \
                 prefix (protontricks <appid> d3dcompiler_47; Install does this for a Steam \
                 game): with the game's copy gone Wine falls back to its own builtin one, \
                 which cannot build the pass either."
            } else {
                ""
            }
        )));
    }
    // The same failure under Proton wears a different face: the add-on compiles
    // its NR "proxy encode" pass at runtime through d3dcompiler_47, and Wine's
    // builtin one (backed by vkd3d-shader's still-incomplete HLSL compiler) does
    // not implement every intrinsic — "isnan" among them — so the compile aborts
    // with E5005 and neural rendering never binds, though every other step reads
    // fine. Microsoft's real d3dcompiler_47 knows the intrinsic; put it in the
    // game's Proton prefix. (This tool installs it at setup from this version on.)
    else if let Some(line) = nr_pass_failed.filter(|_| !wine_hlsl) {
        out.push(bad(format!(
            "{} — the add-on's neural-rendering shader could not be compiled by the HLSL \
             compiler in this process. Under Proton that is Wine's builtin d3dcompiler_47, \
             which does not implement every intrinsic the pass uses. Install Microsoft's real \
             one into this game's prefix -- `protontricks <appid> d3dcompiler_47` (or run \
             Install again, which now does this for you) -- and start the game again.",
            line.trim()
        )));
    }

    // A game with more than one executable (a Vulkan build and a DX11 build,
    // a launcher and the game) can be installed for one and played through
    // another: ReShade loads, everything looks right, nothing is hooked (#33).
    if let Some(loaded) = reshade_host_exe(&rs).filter(|_| !st.is32()) {
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
            // Where the redist actually is decides what the user can do. Unreal
            // puts it in a D3D12 subfolder; a Unity player declares the exe's own
            // folder, so renaming a "D3D12 folder" that was never there changes
            // nothing and reads as a dead end (dlss5-bridge#24).
            let where_ = match game::has_agility_redist(d) {
                Some(p) => {
                    let ver = crate::ngx::file_version(&p).unwrap_or_else(|| "unknown".into());
                    format!(
                        "The copy in force here is {} ({ver}). Rename it and start the game \
                         again: it falls back to the Windows runtime, which every device in \
                         the process can match. If the game will not start without it, verify \
                         the game files instead -- a D3D12Core.dll replaced or truncated by \
                         another tool gives exactly this error.",
                        p.display()
                    )
                }
                None => "No D3D12Core.dll is next to the exe or in a D3D12 folder here, so the \
                         declaration points somewhere else or the file is missing outright. \
                         Verify the game files."
                    .into(),
            };
            out.push(bad(format!(
                "{} — 0x887E0003 is D3D12_ERROR_INVALID_REDIST: the executable declares its own \
                 DirectX 12 Agility SDK (D3D12SDKVersion/D3D12SDKPath exports), and until that \
                 declaration is satisfied no D3D12 device can be created in this process at \
                 all -- not the bridge's, not the game's. Not something this tool sets. {}",
                line.trim(),
                where_
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
                     to refresh it (Install updates an existing Feeder)."
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
        // Some games refuse the reduced work-resolution path: the feed builds
        // its shared textures, the staging SRV for the smaller image fails, and
        // three failed builds stop the feed. Nothing downstream then happens —
        // the add-on's overlay says "HOOKS ARMED - NO DLSS CREATE SEEN" and
        // toggling neural rendering in game does nothing, which reads like the
        // add-on is broken rather than one setting being wrong (#74).
        // The 32-bit halves talk a versioned protocol. When they disagree the
        // host exits immediately and the game shows nothing at all (#69).
        if let Some(line) = fd
            .lines()
            .chain(
                read(&st.consumer_dir(), "dlss5-feed-host.log")
                    .as_deref()
                    .unwrap_or("")
                    .lines(),
            )
            .find(|l| l.contains("speaks protocol v") && l.contains("this host v"))
        {
            out.push(bad(format!(
                "The two halves of the 32-bit install are from different releases: {} \
                 Run Install again: both halves now come from one download and each records \
                 the release it came from, so this cannot happen silently.",
                line.trim()
            )));
        }
        // The host warns when the add-on's version and the driver are a pair it
        // has measured failing. Every build of that add-on carries the same
        // embedded FileVersion (0.2026.0828.0517 in 4.55 and 4.70 alike), so
        // the warning fires on the classic build too — the one the host's own
        // text calls a way through. Only the tag this tool recorded can say
        // which build is actually on disk (#69, and upstream DLSS5-Feeder#90).
        if fd.contains("is a combination measured to fail")
            || read(&st.consumer_dir(), "dlss5-feed-host.log")
                .as_deref()
                .is_some_and(|l| l.contains("is a combination measured to fail"))
        {
            let tag = read(&st.consumer_dir(), crate::game::DLSS5_ADDON_MARKER)
                .map(|t| t.trim().to_owned())
                .unwrap_or_default();
            if tag == crate::installer::RENODX_CLASSIC_TAG {
                out.push(ok(format!(
                    "The host warns that this add-on build and your driver are a combination it \
                     measured failing. You are already on the classic build it recommends \
                     ({tag}) — every build of that add-on reports the same FileVersion, so the \
                     host cannot tell them apart and warns either way. Nothing to do here."
                )));
            } else {
                out.push(warn(format!(
                    "The host measured this add-on build failing on your driver. Run Install \
                     again: it reads that verdict out of this log and pins the classic build \
                     ({}) by itself.",
                    crate::installer::RENODX_CLASSIC_TAG
                )));
            }
        }
        // The create faults inside the driver rather than returning a code. The
        // feed catches it, cannot retry (the consumer's own locks were skipped
        // by the unwind), and stops — so the game runs and nothing happens,
        // with the reason six lines up in a log nobody reads (#76).
        if fd.contains("CreateFeature raised 0xC0000005") {
            let stack = fd
                .lines()
                .find(|l| l.contains("CreateFeature fault stack"))
                .map(|l| l.split("(innermost first):").nth(1).unwrap_or(l).trim())
                .unwrap_or("")
                .to_owned();
            let two_modules = fd.contains("two copies of the DLSS NGX module are loaded");
            let mut t = "The DLSS feature create faulted inside the driver (access violation),                  and the feed stopped: it cannot safely call back in, because the neural                  consumer's own code was on the faulting stack and its locks were skipped by the                  unwind."
                .to_owned();
            if !stack.is_empty() {
                t.push_str(&format!(" Fault stack: {stack}."));
            }
            if two_modules {
                // Tested on Proton and it is the wrong advice there: with the
                // game-local DLL moved aside, the driver's own NGX answers
                // 0xBAD00012 (NotImplemented) for SuperSampling and DLSS is not
                // available at all, which is worse than the crash (#76).
                if wine || fd.contains("driver 999.99") {
                    t.push_str(
                        " On Windows the usual next step is moving the game-local nvngx_dlss.dll \
                         aside, because two copies of the NGX module are loaded and the add-on \
                         hooks both. Do NOT do that here: this is Wine/Proton, where the driver's \
                         own NGX does not provide DLSS, and removing the game-local copy has been \
                         measured to leave DLSS unavailable entirely.",
                    );
                } else {
                    t.push_str(
                        " The log names the most likely cause: two copies of the DLSS NGX module \
                         are loaded — the game-local nvngx_dlss.dll and the driver's own \
                         _nvngx.dll — and the add-on hooks both. Try moving nvngx_dlss.dll out of \
                         the game folder (to nvngx_dlss.dll.off) and starting the game again \
                         WITHOUT re-running Install, which would put it back.",
                    );
                }
            }
            out.push(bad(t));
        }
        if fd.contains("work-resolution staging SRV failed") {
            let pct = fd
                .lines()
                .find(|l| l.contains("work resolution ("))
                .and_then(|l| l.split("work resolution (").nth(1))
                .and_then(|r| r.split(')').next())
                .unwrap_or("below 100%")
                .to_owned();
            out.push(bad(format!(
                "The feed could not build its textures at {pct} of the frame: \
                 \"work-resolution staging SRV failed\", three times, and then it stopped. This \
                 game does not accept the reduced work-resolution path. Set it back to full size \
                 — Settings ▸ Feeder knobs ▸ work_resolution = 100 and work_upscale = 0, or pick \
                 the High quality preset — and start the game again. Everything downstream of this \
                 (no DLSS create, the in-game toggle doing nothing) follows from it."
            )));
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
    /// The game's newest Proton-prefix crash report has a callstack dominated by
    /// the DLSS 5 neural-rendering runtime (`nvngx_dlssnr`) reached through the
    /// add-on — the signed NR runtime faulting under Proton. Carries the crash's
    /// error line. `None` = no such crash found.
    pub nr_runtime_crash: Option<String>,
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
    // The signed NR runtime crashing under Proton is invisible in ReShade.log
    // (it takes the process down mid-frame); the evidence is the game's own UE
    // crash report in the prefix. The callstack is nvngx_dlssnr calling into
    // nvapi64 — Proton's dxvk-nvapi, a reimplementation — and the closed runtime
    // dereferencing what comes back as null (reading 0x18). It can fault within
    // seconds of neural rendering starting, with or without Frame Generation;
    // DLAA/native NR runs cleanly right up to the fault.
    if let Some(err) = &ctx.nr_runtime_crash {
        out.push(bad(format!(
            "The DLSS 5 neural-rendering runtime crashed under Proton: the game's crash report \
             faults inside nvngx_dlssnr via nvapi64 ({err}), reached through the add-on. The \
             signed runtime and Proton's nvapi (dxvk-nvapi) do not fully agree here, and it can \
             go down within seconds of neural rendering starting — with or without DLSS Frame \
             Generation. Worth trying, cheapest first: turn Frame Generation off and keep DLSS \
             on DLAA; update Proton (a newer dxvk-nvapi may not hit it); or switch to the \
             OptiScaler engine (--engine=opti). If it keeps crashing, Remove neural rendering \
             for this title — it is the signed runtime under Proton, not the tool's setup.",
        )));
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
            nr_runtime_crash: None,
        }
    }

    #[test]
    fn host_findings_reports_nr_runtime_crash_and_points_at_frame_generation() {
        let (_t, exe) = setup(true);
        let st = game::inspect(&exe).unwrap();
        let mut ctx = host_ctx();
        ctx.nr_runtime_crash =
            Some("Unhandled Exception: EXCEPTION_ACCESS_VIOLATION reading address 0x18".into());
        let f = host_findings(&st, &ctx);
        let bad = f
            .iter()
            .find(|x| x.level == Level::Bad && x.text.contains("neural-rendering runtime crashed"))
            .expect("crash finding present");
        assert!(bad.text.contains("Frame Generation") && bad.text.contains("DLAA"));
        // Silent when there is no such crash.
        assert!(!host_findings(&st, &host_ctx())
            .iter()
            .any(|x| x.text.contains("neural-rendering runtime crashed")));
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

    #[test]
    fn optiscaler_gpu_device_lost_is_named_with_the_model_res_fix() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = game::testutil::make_pe_importing_padded(&d.join("game.exe"), &["d3d12.dll"], 2_000_000);
        fs::write(d.join(game::DLSS_DLL), b"x").unwrap(); // native mode
        fs::write(d.join(game::OPTI_MANIFEST), "dxgi.dll\nOptiScaler.ini\n").unwrap();
        fs::write(
            d.join("OptiScaler.log"),
            "[I] DlssNr_Dx12::Dispatch DLSS-NR running at 3840x2160, guides 3648x2052\n\
             [I] DlssNr_Dx12::Dispatch DLSS-NR composition: model 3840x2160\n\
             [E] Vulkan_wDx12::hk_vkQueueSubmit vkQueueSubmit failed with error code: VK_ERROR_DEVICE_LOST\n",
        )
        .unwrap();
        let st = game::inspect(&exe).unwrap();
        assert!(st.opti);
        let d2 = diagnose(&st);
        let crash = d2
            .iter()
            .find(|x| x.level == Level::Bad && x.text.contains("GPU crashed"))
            .expect("names the GPU crash");
        // It names the resolution and points at the model-resolution fix.
        assert!(crash.text.contains("3840x2160"), "{}", crash.text);
        assert!(crash.text.contains("--model-res"), "{}", crash.text);
        // It does NOT wrongly say "ReShade never loaded".
        assert!(!d2.iter().any(|x| x.text.contains("ReShade never loaded")));

        // A clean OptiScaler run reads as running, not crashed.
        fs::write(
            d.join("OptiScaler.log"),
            "[I] DlssNr_Dx12::Dispatch DLSS-NR composition: model 1920x1080\n",
        )
        .unwrap();
        let d3 = diagnose(&game::inspect(&exe).unwrap());
        assert!(d3.iter().any(|x| x.level == Level::Ok && x.text.contains("neural rendering is running")));
    }

    /// The missing-bridge case is owned by diagnose() (needs a ReShade.log to
    /// establish "no NGX call"); host_findings only confirms an installed one.
    #[test]
    fn native_dx11_missing_bridge_is_caught_by_diagnose() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = game::testutil::make_pe_importing_padded(
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

    /// A title that rejects the upscaling NR evaluate but not the native one
    /// logs both a success and a failure; the failure (the black frame the
    /// player sees) must win, and name the DLAA workaround.
    #[test]
    fn nr_evaluate_failure_beats_earlier_success_and_names_dlaa() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = game::testutil::make_pe_importing_padded(
            &t.path().join("game.exe"),
            &["d3d12.dll"],
            2_000_000,
        );
        fs::write(t.path().join(game::DLSS_DLL), b"x").unwrap(); // native mode
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n\
             Registered add-on \"DLSS 5 Neural Rendering\"\n\
             DLSS5 Generic: inline feature 18 evaluation succeeded\n\
             DLSS5 Generic: feature 18 evaluate failed with 0xbad00005; NR upscaling is blocked\n",
        )
        .unwrap();
        let st = game::inspect(&exe).unwrap();
        let d = diagnose(&st);
        // The failure is reported, at Bad, with the 0xbad00005/Proton reason and DLAA advice.
        let f = d
            .iter()
            .find(|x| x.text.contains("evaluate failed"))
            .expect("failure finding present");
        assert_eq!(f.level, Level::Bad);
        assert!(f.text.contains("InvalidParameter") && f.text.contains("DLAA"));
        // The optimistic "Neural rendering ran" line must not also appear.
        assert!(!d.iter().any(|x| x.text.contains("Neural rendering ran")));
    }

    /// A 32-bit game runs the add-on under the 64-bit ReShade in host64\, so
    /// that is the log to read. Reading the game folder's log — the feeder's
    /// own 32-bit ReShade, which never loads the add-on — reported "the add-on
    /// never registered" on installs that were fine (#69).
    #[test]
    fn thirty_two_bit_reads_the_host64_reshade_log() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let host = t.path().join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        // The 32-bit ReShade beside the exe: no add-on, and never will have one.
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n",
        )
        .unwrap();
        // The one that matters, in host64\.
        fs::write(
            host.join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Ok && x.text.contains("add-on registered")),
            "{f:?}"
        );
        assert!(
            !f.iter().any(|x| x.text.contains("never registered")),
            "{f:?}"
        );
    }

    /// And when host64\ has no ReShade log at all, say so in host64 terms
    /// rather than claiming ReShade never loaded beside the exe.
    #[test]
    fn thirty_two_bit_missing_host64_log_names_host64() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f.iter().any(|x| x.text.contains("host64")), "{f:?}");
    }

    /// Under Proton the effects are compiled by Wine's d3dcompiler_47
    /// (vkd3d-shader), which has not implemented [fastopt]. Saying "the add-on
    /// never registered" or "rename your d3dcompiler" sends the user in the
    /// wrong direction — the second one actively removes the working compiler (#70).
    #[test]
    fn wine_hlsl_compiler_is_named_and_rename_advice_suppressed() {
        let (t, exe) = setup(true);
        fs::write(t.path().join("d3dcompiler_47.dll"), b"MZ").unwrap();
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n\
             ERROR | Failed to compile 'DLSS5_Feed.fx':\n\
             <anonymous>:118:13: E5017: Aborting due to not yet implemented feature: Unhandled attribute 'fastopt'.\n",
        )
        .unwrap();
        // A Windows build reading a Wine session's log: the log gives it away.
        let f = diagnose_with(&game::inspect(&exe).unwrap(), false);
        assert!(
            f.iter()
                .any(|x| x.level == Level::Bad && x.text.contains("vkd3d-shader")),
            "{f:?}"
        );
        assert!(
            !f.iter().any(|x| x.text.contains("d3dcompiler_47.dll.bak")),
            "{f:?}"
        );
        // The recipe a reporter measured working, not just "install a compiler" (#76).
        assert!(
            f.iter().any(|x| x.text.contains("WINEDLLOVERRIDES")),
            "{f:?}"
        );
    }

    /// Dying Light refuses the reduced work-resolution path. The user sees a
    /// stopped feed and an add-on saying it never saw a DLSS create, with
    /// nothing naming the one setting responsible (#74).
    #[test]
    fn work_resolution_build_failure_names_the_setting() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] building: 2176x1224 work resolution (85%) -> 2560x1440 backbuffer\n\
             [feed] work-resolution staging SRV failed\n\
             [feed] failure: resource build\n\
             stopped: repeated failures. The game renders normally.\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        let hit = f
            .iter()
            .find(|x| x.text.contains("work-resolution staging SRV failed"))
            .unwrap_or_else(|| panic!("{f:?}"));
        assert_eq!(hit.level, Level::Bad);
        assert!(hit.text.contains("85%"), "{}", hit.text);
        assert!(hit.text.contains("work_resolution = 100"), "{}", hit.text);
    }

    /// "Home does nothing" on a 32-bit game means the ReShade beside the exe is
    /// missing, which is upstream of every host64 finding. Diagnose used to
    /// report only the host64 side and leave the player looking in the wrong
    /// folder (#69).
    #[test]
    fn thirty_two_bit_missing_game_side_reshade_is_named_first() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let host = t.path().join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        fs::write(
            host.join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Bad && x.text.contains("No ReShade beside the game exe")),
            "{f:?}"
        );
    }

    /// A create that faults inside the driver stops the feed for good, and the
    /// log's own hint about two NGX modules is the actionable part. Neither
    /// reached the user (#76).
    #[test]
    fn a_faulting_feature_create_is_explained_with_its_stack() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] CreateFeature raised 0xC0000005 (reading address 00000000575284C0) (caught; nothing submitted)\n\
             [feed] CreateFeature fault stack, by module (innermost first): d3d12core.dll <- dxgi.dll <- nvapi64.dll <- nvngx_dlss.dll <- renodx-dlss5.addon64\n\
             [feed] two copies of the DLSS NGX module are loaded (the game-local nvngx_dlss.dll and the driver's _nvngx.dll)\n",
        )
        .unwrap();
        // On Windows the advice is to move the game-local copy aside.
        let f = diagnose_with(&game::inspect(&exe).unwrap(), false);
        let hit = f
            .iter()
            .find(|x| x.text.contains("faulted inside the driver"))
            .unwrap_or_else(|| panic!("{f:?}"));
        assert_eq!(hit.level, Level::Bad);
        assert!(hit.text.contains("d3d12core.dll"), "{}", hit.text);
        assert!(hit.text.contains("nvngx_dlss.dll.off"), "{}", hit.text);
        // A Linux build knows it is looking at Proton, where that advice is wrong.
        let f = diagnose_with(&game::inspect(&exe).unwrap(), true);
        assert!(!f.iter().any(|x| x.text.contains("nvngx_dlss.dll.off")), "{f:?}");
    }

    /// GTA IV reads the adapter's VRAM and sizes its pools from it, so dgVoodoo
    /// reporting 4096 MB without a matching -availablevidmem is a black screen
    /// after load, and no conf at all is TEXP60 at startup (#69).
    #[test]
    fn dgvoodoo_vram_without_availablevidmem_is_flagged() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\n",
        )
        .unwrap();
        fs::write(t.path().join("dgVoodoo.conf"), "[DirectX]\nVRAM = 4096\n").unwrap();
        fs::write(t.path().join("commandline.txt"), "-norestrictions\n").unwrap();
        let f = run(&exe).unwrap();
        let hit = f
            .iter()
            .find(|x| x.text.contains("-availablevidmem"))
            .unwrap_or_else(|| panic!("{f:?}"));
        assert_eq!(hit.level, Level::Warn);
        assert!(hit.text.contains("4096"), "{}", hit.text);

        // Already set: nothing to say.
        fs::write(
            t.path().join("commandline.txt"),
            "-availablevidmem 4032\n-norestrictions\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            !f.iter().any(|x| x.text.contains("-availablevidmem")),
            "{f:?}"
        );

        // Found whatever its casing on disk (ext4 is case-sensitive).
        fs::remove_file(t.path().join("commandline.txt")).unwrap();
        fs::write(t.path().join("CommandLine.txt"), "-norestrictions\n").unwrap();
        let f = run(&exe).unwrap();
        assert!(f.iter().any(|x| x.text.contains("-availablevidmem")), "{f:?}");
    }

    /// A game's own d3dcompiler_47.dll is worth renaming on Windows when it is
    /// too old, and exactly wrong to touch under Proton, where it is
    /// Microsoft's compiler standing in for Wine's builtin one.
    #[test]
    fn a_game_compiler_is_left_alone_under_proton() {
        let (t, exe) = setup(true);
        fs::write(t.path().join("D3DCompiler_47.dll"), b"MZ").unwrap();
        fs::write(t.path().join("ReShade.log"), "Initializing crosire's ReShade\n").unwrap();
        let st = game::inspect(&exe).unwrap();
        let rename = |f: &[Finding]| f.iter().any(|x| x.text.contains("d3dcompiler_47.dll.bak"));
        assert!(rename(&diagnose_with(&st, false)), "found despite its casing");
        assert!(!rename(&diagnose_with(&st, true)));
    }

    /// Wine's compiler failing both the effects and the add-on's neural pass is
    /// one cause with one fix, so it is one finding that names both.
    #[test]
    fn wine_compiler_failures_are_one_finding() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\n\
             <anonymous>:118:13: E5017: Aborting due to not yet implemented feature: Unhandled attribute 'fastopt'.\n\
             renodx-dlss5: proxy encode compilation failed: E5005: Function \"isnan\" is not defined.\n",
        )
        .unwrap();
        let f = diagnose_with(&game::inspect(&exe).unwrap(), true);
        let hits: Vec<_> = f.iter().filter(|x| x.text.contains("d3dcompiler_47")).collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].text.contains("isnan"), "{}", hits[0].text);
    }

    /// Moving the game-local nvngx_dlss.dll aside is the right advice on
    /// Windows and the wrong advice under Proton, where the driver's own NGX
    /// answers NotImplemented and DLSS disappears entirely. 0batsy tested both
    /// and neither helped, but the second left him worse off (#76).
    #[test]
    fn the_nvngx_advice_is_withheld_under_proton() {
        let (t, exe) = setup(true);
        let crash = "[feed] CreateFeature raised 0xC0000005 (caught; nothing submitted)\n\
                     [feed] two copies of the DLSS NGX module are loaded (the game-local nvngx_dlss.dll and the driver's _nvngx.dll)\n";
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();

        // Windows: the advice stands.
        fs::write(t.path().join("dlss5-feed.log"), crash).unwrap();
        let f = diagnose_with(&game::inspect(&exe).unwrap(), false);
        assert!(
            f.iter().any(|x| x.text.contains("nvngx_dlss.dll.off")),
            "{f:?}"
        );

        // Proton, identified by the adapter line vkd3d reports.
        fs::write(
            t.path().join("dlss5-feed.log"),
            format!("[feed] adapter: NVIDIA GeForce RTX 4070 SUPER driver 999.99\n{crash}"),
        )
        .unwrap();
        let f = diagnose_with(&game::inspect(&exe).unwrap(), false);
        assert!(
            !f.iter().any(|x| x.text.contains("nvngx_dlss.dll.off")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|x| x.text.contains("Do NOT do that here")),
            "{f:?}"
        );

        // A Linux build needs no clue in the log: every game it sees is Proton.
        fs::write(t.path().join("dlss5-feed.log"), crash).unwrap();
        let f = diagnose_with(&game::inspect(&exe).unwrap(), true);
        assert!(!f.iter().any(|x| x.text.contains("nvngx_dlss.dll.off")), "{f:?}");
        assert!(f.iter().any(|x| x.text.contains("Do NOT do that here")), "{f:?}");
    }

    /// The host's warning keys on a FileVersion every build of that add-on
    /// shares, so it fires on the classic build the host itself recommends.
    /// Only the tag this tool recorded can tell them apart (#69).
    #[test]
    fn the_measured_to_fail_warning_reads_the_recorded_tag() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] WARNING: renodx-dlss5 v4.6 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();

        // No tag on disk: the advice is to re-run Install, which pins it.
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Warn && x.text.contains("pins the classic build")),
            "{f:?}"
        );

        // Already pinned: say so instead of sending them round again.
        fs::write(
            t.path().join(crate::game::DLSS5_ADDON_MARKER),
            crate::installer::RENODX_CLASSIC_TAG,
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Ok && x.text.contains("cannot tell them apart")),
            "{f:?}"
        );
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
