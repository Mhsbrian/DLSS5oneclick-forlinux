#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod game;
mod gpu;
mod gui;
mod installer;
mod logo;
mod net;
mod reshade_ini;
mod theme;

use std::io::Write;
use std::path::PathBuf;

/// `dlss5oneclick <GAME.exe | game folder> [--remove | --remove-all | --check | --engine=opti]` runs headless; no args opens the GUI.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first().filter(|a| !a.starts_with('-')) {
        attach_parent_console();
        let code = cli(
            PathBuf::from(first),
            args.iter().any(|a| a == "--remove"),
            args.iter().any(|a| a == "--remove-all"),
            args.iter().any(|a| a == "--check"),
            if args.iter().any(|a| a == "--engine=opti" || a == "--opti") {
                installer::Engine::Opti
            } else {
                installer::Engine::ReShade
            },
        );
        std::process::exit(code);
    }
    if let Err(e) = gui::run() {
        eprintln!("gui error: {e}");
        std::process::exit(2);
    }
}

fn cli(
    target: PathBuf,
    remove: bool,
    remove_all: bool,
    check: bool,
    engine: installer::Engine,
) -> i32 {
    let (exe, candidates) = match game::resolve_target(&target) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 1;
        }
    };
    if candidates.len() > 1 {
        let others: Vec<String> = candidates[1..]
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        println!(
            "using {} (other candidates: {})",
            exe.display(),
            others.join(", ")
        );
    } else if !candidates.is_empty() {
        println!("using {}", exe.display());
    }
    if check {
        return match game::inspect(&exe) {
            Ok(st) => {
                println!(
                    "{} | {}-bit | {} | mode={:?} | reshade={} headers={} feeder={} lumenite={} dlss5={} dlssnr={} dlss={} bridge={} opti={} | gpu={} | complete={}",
                    exe.display(), st.bitness, st.api.label(), st.mode, st.reshade, st.headers, st.feeder,
                    st.lumenite, st.dlss5_addon, st.dlssnr, st.dlss,
                    st.bridge,
                    st.opti,
                    st.gpu.as_ref().map(|(g, t)| format!("{} [{}]", g.name, t.label())).unwrap_or_else(|| "unknown".into()),
                    st.complete()
                );
                for p in &st.problems {
                    println!("  ! {p}");
                }
                let names: Vec<&str> = installer::plan(&st).iter().map(|s| s.name).collect();
                println!("  plan: {}", names.join(" -> "));
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
    }
    if remove_all {
        return match installer::uninstall_all(&exe) {
            Ok((list, kept)) => {
                for f in list {
                    println!("removed {f}");
                }
                if let Some(k) = kept {
                    println!("{k}");
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
    }
    if remove {
        return match installer::uninstall(&exe) {
            Ok(list) => {
                for f in list {
                    println!("removed {f}");
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
    }
    let progress = |pct: u8, msg: &str| {
        print!("\r{pct:3}% {msg:<72}");
        let _ = std::io::stdout().flush();
    };
    let step = move |i: usize, n: usize, name: &str, state: installer::StepState, detail: &str| {
        use installer::StepState::*;
        match state {
            Start => println!("\n[{}/{n}] {name}", i + 1),
            Done => println!("\n      ok: {detail}"),
            Error => println!("\n      FAILED: {detail}"),
        }
    };
    match installer::run_all_with(&exe, engine, &progress, &step) {
        Ok(_) => {
            if engine == installer::Engine::Opti {
                println!(
                    "
Done. In game: Insert opens the OptiScaler overlay -> enable Neural Rendering (off by default)."
                );
            } else {
                println!(
                    "
Done. In game: Home -> DLSS 5 Neural Rendering panel -> enable."
                );
            }
            0
        }
        Err(e) => {
            eprintln!("\nerror: {e:#}");
            1
        }
    }
}

/// Release builds hide the console; when launched from a terminal, reattach so CLI output shows.
fn attach_parent_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
