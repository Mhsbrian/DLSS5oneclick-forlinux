#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod diagnose;
mod game;
mod gpu;
mod gui;
mod installer;
mod logo;
mod net;
mod platform;
mod reshade_ini;
mod theme;
mod update;

use std::io::Write;
use std::path::PathBuf;

/// `dlss5oneclick <GAME.exe | game folder | game name | appid> [--remove | --remove-all |
/// --check | --diagnose | --engine=opti | --launch-options | --revert-launch-options]
/// | --list-games | --update` runs headless; no args opens the GUI.
fn main() {
    update::cleanup_old();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--fetch") {
        attach_parent_console();
        let (Some(url), Some(dest)) = (args.get(1), args.get(2)) else {
            eprintln!("usage: --fetch <url> <file>");
            std::process::exit(2);
        };
        let code = match net::client().and_then(|c| {
            net::download(&c, url, std::path::Path::new(dest), "fetch", &|p, m| {
                print!("\r{p:3}% {m:<72}");
                let _ = std::io::stdout().flush();
            })
        }) {
            Ok(()) => {
                println!(
                    "
ok"
                );
                0
            }
            Err(e) => {
                eprintln!(
                    "
error: {e:#}"
                );
                1
            }
        };
        std::process::exit(code);
    }
    if args.iter().any(|a| a == "--update") {
        attach_parent_console();
        std::process::exit(cli_update());
    }
    if args.iter().any(|a| a == "--list-games") {
        attach_parent_console();
        std::process::exit(cli_list_games());
    }
    if let Some(first) = args.first().filter(|a| !a.starts_with('-')) {
        attach_parent_console();
        let target = PathBuf::from(first);
        let target = if target.exists() {
            target
        } else {
            match resolve_game_arg(first) {
                Ok(dir) => dir,
                Err(msg) => {
                    eprintln!("{msg}");
                    std::process::exit(2);
                }
            }
        };
        let code = cli(
            target,
            args.iter().any(|a| a == "--remove"),
            args.iter().any(|a| a == "--remove-all"),
            args.iter().any(|a| a == "--check"),
            args.iter().any(|a| a == "--diagnose"),
            if args.iter().any(|a| a == "--engine=opti" || a == "--opti") {
                installer::Engine::Opti
            } else {
                installer::Engine::ReShade
            },
            if args.iter().any(|a| a == "--revert-launch-options") {
                Some(true)
            } else if args.iter().any(|a| a == "--launch-options") {
                Some(false)
            } else {
                None
            },
        );
        std::process::exit(code);
    }
    if let Err(e) = gui::run() {
        eprintln!("gui error: {e}");
        std::process::exit(2);
    }
}

fn cli_update() -> i32 {
    match update::check() {
        Ok(None) => {
            println!("DLSS5oneclick {} is the latest version.", update::CURRENT);
            0
        }
        Ok(Some(av)) => {
            println!(
                "{} -> {} available. Downloading...",
                update::CURRENT,
                av.version
            );
            let progress = |pct: u8, msg: &str| {
                print!("\r{pct:3}% {msg:<72}");
                let _ = std::io::stdout().flush();
            };
            match update::download_and_swap(&av, &progress) {
                Ok(exe) => {
                    println!("\nUpdated to {} at {}", av.version, exe.display());
                    0
                }
                Err(e) => {
                    eprintln!("\nerror: {e:#}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

fn cli_list_games() -> i32 {
    let games = platform::scan_all();
    if games.is_empty() {
        println!("No games found through Steam, Heroic or Lutris.");
        return 0;
    }
    for g in &games {
        println!(
            "[{:<6}] {:<40} ({})  {}",
            g.launcher.label(),
            g.name,
            g.id,
            g.dir.display()
        );
    }
    0
}

/// A non-path target: exact launcher id (Steam appid) first, then a
/// case-insensitive name substring across every known launcher.
fn resolve_game_arg(arg: &str) -> Result<PathBuf, String> {
    let games = platform::scan_all();
    if let Some(g) = games.iter().find(|g| g.id == arg) {
        return Ok(g.dir.clone());
    }
    let needle = arg.to_lowercase();
    let hits: Vec<_> = games
        .iter()
        .filter(|g| g.name.to_lowercase().contains(&needle))
        .collect();
    match hits.len() {
        0 => Err(format!(
            "not found: {arg} (no such path, appid or game name; --list-games shows what is known)"
        )),
        1 => {
            println!("{}: {}", hits[0].name, hits[0].dir.display());
            Ok(hits[0].dir.clone())
        }
        _ => Err(format!(
            "ambiguous game name {:?}: {}",
            arg,
            hits.iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Render a `LaunchAdvice` for the terminal; nonzero when the user still has
/// to act by hand.
#[cfg(target_os = "linux")]
fn print_advice(advice: &platform::LaunchAdvice) -> i32 {
    use platform::LaunchAdvice as A;
    match advice {
        A::AppliedSteam(outcomes) => {
            println!("\nSteam launch options set:");
            for o in outcomes {
                println!("  {}", o.merged);
                println!(
                    "  (edited {}; backup: {})",
                    o.file.display(),
                    o.backup.display()
                );
            }
            0
        }
        A::AlreadySet => {
            println!("\nSteam launch options already contain everything needed.");
            0
        }
        A::ManualSteam { display, why } => {
            println!("\nCould not set Steam launch options automatically: {why}");
            println!("Set them yourself: right-click the game -> Properties -> Launch Options:");
            println!("  {display}");
            1
        }
        A::AppliedHeroic { file } => {
            println!(
                "\nHeroic environment variables set ({}). Restart Heroic to pick them up.",
                file.display()
            );
            0
        }
        A::ManualEnv {
            launcher,
            vars,
            why,
        } => {
            if let Some(why) = why {
                println!("\n{why}");
            }
            println!(
                "\nAdd these environment variables in {}'s settings for this game:",
                launcher.label()
            );
            for (k, v) in vars {
                println!("  {k}={v}");
            }
            1
        }
        A::UnknownLauncher { display } => {
            println!(
                "\nThis folder belongs to no known launcher. Under Proton/Wine the game needs:"
            );
            println!("  {display}");
            println!("(Steam: Properties -> Launch Options. Lutris/Heroic: per-game environment variables.)");
            1
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cli(
    target: PathBuf,
    remove: bool,
    remove_all: bool,
    check: bool,
    diagnose_only: bool,
    engine: installer::Engine,
    launch_only: Option<bool>,
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
    if let Some(_revert) = launch_only {
        #[cfg(target_os = "linux")]
        {
            let d = exe.parent().map(std::path::Path::to_path_buf).unwrap_or(exe);
            return print_advice(&platform::ensure_launch_options(&d, engine, _revert));
        }
        #[cfg(not(target_os = "linux"))]
        {
            eprintln!("--launch-options / --revert-launch-options only apply on Linux.");
            return 2;
        }
    }
    if diagnose_only {
        return match diagnose::run_full(&exe) {
            Ok(findings) => {
                for f in &findings {
                    let tag = match f.level {
                        diagnose::Level::Ok => "ok  ",
                        diagnose::Level::Warn => "warn",
                        diagnose::Level::Bad => "FAIL",
                    };
                    println!("[{tag}] {}", f.text);
                }
                if findings.iter().any(|f| f.level == diagnose::Level::Bad) {
                    1
                } else {
                    0
                }
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
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
                println!("
Done. In game: Home opens ReShade -> Add-ons tab -> DLSS 5 Neural Rendering -> enable. (Home tab saying no effect files is normal on games with their own DLSS.)");
            }
            #[cfg(target_os = "linux")]
            if let Some(d) = exe.parent() {
                print_advice(&platform::ensure_launch_options(d, engine, false));
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
