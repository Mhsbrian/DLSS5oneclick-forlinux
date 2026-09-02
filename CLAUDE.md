# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Fork context

This repo (`Mhsbrian/DLSS5oneclick-forlinux`) is the **Linux port** of `faisalkindi/DLSS5oneclick`, a Rust tool that automates installing the leaked DLSS 5 neural-rendering build into DX11/DX12 games. The port targets Steam/Proton gaming distros: launcher game discovery (Steam/Heroic/Lutris), automatic Steam launch-option handling, Linux GPU/driver detection, Linux self-update, and binary+AppImage releases from this fork. The crate still compiles on Windows — all Linux integration is `cfg(target_os = "linux")`-gated at the data-source edges, with parsers/mergers platform-neutral — so upstream changes merge cleanly. Windows binaries are not released from this fork (Windows users use upstream).

## Commands

```
cargo test                                  # full suite; no network, no host Steam dependence
cargo test <test_name>                      # single test (tests are inline #[cfg(test)] modules)
cargo clippy --all-targets -- -D warnings   # CI gates on this, on ubuntu AND windows
cargo build --release                       # target/release/dlss5oneclick
cargo run -- --list-games                   # enumerate Steam/Heroic/Lutris games
cargo run -- "<folder | name | appid>" --check   # detect mode/API/plan without installing
packaging/build-appimage.sh                 # AppImage from a release build
```

CI (`.github/workflows/ci.yml`): clippy + test matrix on ubuntu-latest and windows-latest; a `v*` tag builds `dlss5oneclick-linux-x86_64` + the AppImage on ubuntu-22.04 (glibc 2.35 floor) and attaches both to the release. Version lives in `Cargo.toml` and flows into the user agent, self-update comparison, and GUI title.

## Architecture

Single crate, no workspace. `main.rs` dispatches on argv: a path argument runs headless CLI (flags `--check`, `--diagnose`, `--remove`, `--remove-all`, `--engine=opti`); `--update` self-updates; `--fetch <url> <file>` is a download diagnostic; no args opens the egui GUI.

The core pipeline, shared by CLI and GUI:

1. `game::resolve_target` — accepts a folder or exe, finds candidate game exes (searches two levels deep, skips crash handlers/redist installers, prefers `*-Shipping.exe`).
2. `game::inspect` → `GameStatus` — the central struct. Determines `Mode::Native` (game ships its own DLSS: an unmarked `nvngx_dlss.dll`, Streamline `sl.*.dll`, or dlssg/dlssd DLL up to 4 levels deep) vs `Mode::Feeder` (no DLSS); DX11/DX12 from the exe's PE import table (own minimal PE parser in `game.rs`, no external crate; unknown ⇒ assume DX12); which components are already installed; and `problems` — a non-empty `problems` vec is a refusal (anti-cheat detected, non-RTX GPU, 32-bit game, DX9 proxy) and `run_all_with` bails before any network I/O.
3. `installer::plan_with(&status, engine)` → ordered `Vec<Step>`; each `Step` is a named fn pointer `(client, status, workdir, progress) -> Result<Vec<String>>` (files written). Steps are idempotent — a re-run only fetches what is missing. After each step the game folder is re-inspected.

Two engines: `Engine::ReShade` (default; the six Feeder-README steps for no-DLSS games, or ReShade + RenoDX add-on + DX11 bridge for native-DLSS games) and `Engine::Opti` (OptiScaler fork, native-DLSS games only; extracts the release as `dxgi.dll` and records every written path in a `.dlss5oneclick-optiscaler-manifest` so uninstall is exact). Both engines load as `dxgi.dll`, so they are mutually exclusive per game.

The Linux layer lives in `src/platform/`:

- `platform/vdf.rs` — hand-rolled Valve KeyValues parser (ordered, duplicate keys, CI lookups) plus `set_string_preserving`, a byte-preserving splice of exactly one value into `localconfig.vdf`; `equal_except` is the post-edit safety check. The riskiest code in the repo — every edit path is fixture-tested, including the machine-real escaped-quotes LaunchOptions string.
- `platform/steam.rs` — Steam roots (native/Flatpak/Snap, deduped through the `~/.steam/steam` symlink), libraries from `libraryfolders.vdf`, games from `appmanifest_*.acf` (tooling rows filtered), `compatdata`, `proton_for` via `config.vdf` CompatToolMapping (`proton_info` parses GE-Proton/cachyos/UMU/legacy `proton_63`-style names; `nvapi_env_needed` = Proton < 9 or unknown), `is_running` (`/proc/*/comm`), and `apply_with` — the guarded localconfig edit (Steam-closed gate, `.dlss5o.orig`/`.bak` backups, re-parse equivalence check, atomic rename; appid blocks are only created in the most-recently-modified userdata).
- `platform/launch_options.rs` — pure `LaunchReq` engine: `required` (dxgi=n,b always; d3dcompiler_47=n only when the DLL is present; PROTON_ENABLE_NVAPI=1 only for old/unknown Proton), idempotent `merge` (folds into existing WINEDLLOVERRIDES, preserves user tokens and post-`%command%` args), `strip`, `display`, `env_pairs`.
- `platform/heroic.rs` — Epic (`legendaryConfig/legendary/installed.json`) + GOG (`gog_store/installed.json`) enumeration; `apply_env` edits `GamesConfig/<app>.json` accepting three env-key spellings (`enviromentOptions` sic is Heroic's own). Experimental — unverified against a live Heroic.
- `platform/lutris.rs` — list-only indent-aware reader of `games/*.yml` (top-level `game:` block only; nested `script: game:` must be ignored; `exe:` may be prefix-relative). No YAML writes by design.
- `platform/mod.rs` — `GameEntry`/`scan_all`/`entry_for_path` (deepest dir-prefix match maps a manual folder back to its launcher), `ensure_launch_options` → `LaunchAdvice` (rendered by both CLI `print_advice` and the GUI panel), `host_context` → `diagnose::HostContext`.

`gpu.rs` has a real Linux `list()`: `/proc/driver/nvidia/gpus/*/information` "Model:" lines (falls back to DRM vendor ids in `/sys/class/drm` — AMD/Intel-only ⇒ `NotNvidia` refusal parity; NVIDIA-without-driver ⇒ Unknown, never a false refusal — then `nvidia-smi`); `driver_version()` reads `/sys/module/nvidia/version`. `game.rs` adds `existing_ci`/`join_ci` (case-insensitive lookups that reuse on-disk casing) used at every inspect/install/uninstall boundary — ext4 is case-sensitive and other tools may have created differently-cased trees. `diagnose.rs` splits `run` (log-only, host-independent, what tests use) from `run_full` (host findings first — launch options vs required, NGX Wine DLLs, prefix nvngx, driver, Proton — then logs). `update.rs` self-updates from this fork with cfg'd asset names, ELF validation, chmod 0755, and `$APPIMAGE` redirection (the mounted image is read-only; the AppImage file itself is replaced).

Supporting modules:

- `net.rs` — blocking reqwest. GitHub is queried via the API when `GITHUB_TOKEN` is set, otherwise by scraping the public release HTML pages (avoids the 60/hr unauthenticated API cap). Downloads retry on connection-level failures.
- `reshade_ini.rs` — minimal INI parser/writer that preserves existing user keys and understands ReShade's `,,` comma escape. Ordering matters: `Lumenite_Kernel` must sit above `DLSS5_Feed` in the preset's technique list.
- `update.rs` — self-update without the API: `releases/latest` answers with a 302 whose Location header carries the tag. Swaps the running binary, keeping `<name>.old` until next start (details in the Linux-layer paragraph above).
- `diagnose.rs` — reads `ReShade.log` / `dlss5-feed.log` next to the game exe and emits leveled `Finding`s explaining why neural rendering is or isn't running.
- `gui.rs` — egui/eframe "instrument panel". Installs run on a worker thread; progress reaches the UI over an mpsc channel (`Msg` enum). `theme.rs` / `logo.rs` hold the palette, bundled OFL fonts, and vector-drawn mark.

File-ownership conventions the code relies on: a tool-placed `nvngx_dlss.dll` gets a `nvngx_dlss.dll.dlss5oneclick` sidecar so it is never mistaken for the game's own (that distinction drives Native-vs-Feeder mode detection); `uninstall` keeps ReShade and `nvngx_dlss.dll`, `uninstall_all` removes ReShade only when no foreign add-ons/shaders remain.

## Environment variables

- `DLSS5ONECLICK_SKIP_GPU_CHECK=1` — bypass the GPU refusal; tests set it so they pass on any machine.
- `DLSS5ONECLICK_IGNORE_ANTICHEAT=1` — bypass the anti-cheat refusal.
- `DLSS5ONECLICK_NO_API=1` — force the HTML-page fallback instead of the GitHub API.
- `GITHUB_TOKEN` — used for GitHub API calls when set.

## Tests

Inline `#[cfg(test)]` modules per file; `game::testutil` provides `make_pe` (minimal fake PE with a chosen machine field) and `make_reshade_dll` for building fake game folders in tempdirs. Tests never touch the network — installer tests feed local zips/exes through the same extraction paths the real steps use. Keep new tests on that pattern.
