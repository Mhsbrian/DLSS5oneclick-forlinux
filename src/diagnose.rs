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
fn ngx_init_failure(log: &str, out: &mut Vec<Finding>) {
    let Some(line) = log
        .lines()
        .find(|l| l.contains("NVSDK_NGX") && l.contains("Init") && l.contains("0xBAD00001"))
    else {
        return;
    };
    out.push(bad(format!(
        "NGX refused to initialise: {}. 0xBAD00001 is FeatureNotSupported, which NGX also \
         answers when its runtime is not on the system — not a ReShade, shader or add-on \
         problem. {}. Then update the NVIDIA driver (616.56 or newer) and re-run Install.",
        line.trim(),
        crate::ngx::describe()
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

    // ── DX11 bridge (native DLSS on D3D11) ──────────────────────────
    if let Some(bl) = read(d, "dlss5-bridge.log") {
        if let Some(line) = bl.lines().rev().find(|l| l.contains("stopped:")) {
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
        } else if bl.contains("frames:") && !bl.contains("session failed") {
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
        ngx_init_failure(&fd, &mut out);
        // 32-bit games: the work happens in host64\, and its own log names the reason.
        if let Some(hl) = read(&d.join(game::HOST_DIR), "dlss5-feed-host.log") {
            if hl.contains("feature ready") {
                out.push(ok(
                    "The host64 helper built its DLSS feature (feature ready … DLAA).",
                ));
            }
            ngx_init_failure(&hl, &mut out);
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
pub fn run(exe: &Path) -> Result<Vec<Finding>> {
    let st = game::inspect(exe)?;
    Ok(diagnose(&st))
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
