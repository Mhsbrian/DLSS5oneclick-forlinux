#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod game;
mod gui;
mod installer;
mod net;
mod reshade_ini;

use std::io::Write;
use std::path::PathBuf;

/// `dlss5oneclick <GAME.exe> [--remove]` runs headless; no args opens the GUI.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first().filter(|a| !a.starts_with('-')) {
        attach_parent_console();
        let code = cli(PathBuf::from(first), args.iter().any(|a| a == "--remove"));
        std::process::exit(code);
    }
    if let Err(e) = gui::run() {
        eprintln!("gui error: {e}");
        std::process::exit(2);
    }
}

fn cli(exe: PathBuf, remove: bool) -> i32 {
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
    let n = installer::STEPS.len();
    let step = move |i: usize, name: &str, state: installer::StepState, detail: &str| {
        use installer::StepState::*;
        match state {
            Start => println!("\n[{}/{n}] {name}", i + 1),
            Done => println!("\n      ok: {detail}"),
            Error => println!("\n      FAILED: {detail}"),
        }
    };
    match installer::run_all(&exe, &progress, &step) {
        Ok(_) => {
            println!("\nDone. In game: Home -> DLSS 5 Neural Rendering panel -> enable.");
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
