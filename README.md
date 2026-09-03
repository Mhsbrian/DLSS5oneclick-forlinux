# DLSS5oneclick for Linux

One button that sets up the **leaked DLSS 5 neural-rendering build** in DirectX 11/12 games running under **Steam/Proton** (Heroic and Lutris games work too). This is the Linux port of [faisalkindi/DLSS5oneclick](https://github.com/faisalkindi/DLSS5oneclick) — same installer, plus everything a Linux gaming box actually needs: your Steam library listed in the app, the Proton launch options set for you, Linux GPU/driver checks, and native Linux builds.

Single native binary, no runtime. Everything it installs is downloaded from the projects that made it; the only third-party content inside the binary is three SIL-OFL fonts.

Download: [latest release](https://github.com/Mhsbrian/DLSS5oneclick-forlinux/releases/latest) → `dlss5oneclick-linux-x86_64` (portable binary) or `dlss5oneclick-x86_64.AppImage`.

```
chmod +x dlss5oneclick-linux-x86_64
./dlss5oneclick-linux-x86_64
```

Works on any x86_64 gaming distro with glibc ≥ 2.35 (Arch/CachyOS/EndeavourOS, Bazzite, Nobara, Fedora, Ubuntu/Mint/Pop!_OS, openSUSE, SteamOS desktop mode). Wayland and X11 both fine; the file picker uses the XDG portal.

## What the Linux version adds

- **Game library built in.** Your installed Steam games (native, Flatpak or Snap Steam, all library folders), Heroic (Epic/GOG) and Lutris games are listed in the app — click one instead of hunting for the folder. `--list-games` does the same in the terminal, and the CLI takes a game name or Steam appid: `dlss5oneclick "skyrim special"`.
- **Launch options handled.** ReShade/OptiScaler load as `dxgi.dll`, which Proton only picks up with `WINEDLLOVERRIDES="dxgi=n,b" %command%` in the game's launch options. After an install the tool sets this **for you** (Steam closed: it edits `localconfig.vdf` surgically, with backups; Steam running: it shows the exact string with a copy button). Existing launch options are merged, never overwritten — your `MANGOHUD=1`, game arguments, and other DLL overrides survive. `--launch-options` / `--revert-launch-options` do it from the terminal. On old Proton (< 9) `PROTON_ENABLE_NVAPI=1` is added too; Heroic gets its per-game environment variables written (experimental), Lutris users get the exact variables to paste.
- **Linux GPU and driver checks.** The RTX tier (RTX 50 full speed · RTX 40 moderate cost · RTX 20/30 heavy cost) is read from the NVIDIA driver; non-NVIDIA and GTX cards are refused up front, exactly like on Windows. `--diagnose` also verifies the driver's Wine NGX DLLs (`/usr/lib/nvidia/wine`), the kernel driver version, the game's Proton mapping and its prefix — the usual "DLSS silently does nothing" causes on Linux.
- **Self-updating from this repo**, AppImage included.

Everything else is upstream (currently merged at 0.10.2), unchanged: the two install paths, the OptiScaler engine, the games grid with posters, optional per-game **RenoDX HDR mods** (`--renodx`), automatic **REFramework** for RE Engine games, component refresh on reinstall, 32-bit games via the host64 helper, the detection override (`--mode=feeder|native`), and `--check` / `--remove` / anti-cheat refusals (`--ignore-anticheat` to override at your own risk). See [upstream's README](https://github.com/faisalkindi/DLSS5oneclick#readme) for the full story of what gets installed and from where.

## Use

1. Run the binary. The **Games** page lists your installed games with artwork; pick one (or **Add a folder** / **Add a game** for anything else — a hand-added game is remembered in an **Added by you** section; right-click its card to forget it).
2. **Install DLSS 5**.
3. Let it set the launch options (or paste the shown string into Steam → right-click the game → Properties → Launch Options yourself). Restart Steam if it edited the file.
4. In game: **Home** opens ReShade → **Add-ons** tab → **DLSS 5 Neural Rendering** panel → enable it. **F6** toggles, **F5** saves the add-on's screenshot. On the OptiScaler engine: **Insert** opens the overlay instead.

If nothing seems to happen in game, run `--diagnose` (button or CLI) — it reads the logs and the host setup and says exactly what is wrong.

CLI: `dlss5oneclick <folder | name | appid>` installs · `--check` detect only · `--diagnose` · `--remove` / `--remove-all` · `--engine=opti` · `--renodx` · `--mode=feeder|native` · `--ignore-anticheat` · `--launch-options` / `--revert-launch-options` · `--list-games` · `--update`.

## Launch options details

The required string per game is at most:

```
WINEDLLOVERRIDES="d3dcompiler_47=n;dxgi=n,b" PROTON_ENABLE_NVAPI=1 %command%
```

- `dxgi=n,b` — always: loads ReShade/OptiScaler from the game folder.
- `d3dcompiler_47=n` — only when a native `d3dcompiler_47.dll` sits next to the game exe (many games ship one). Without it Proton's builtin compiler is used, which usually handles the shaders fine; if `--diagnose` shows effect-compile failures, drop a native `d3dcompiler_47.dll` next to the exe (e.g. via `winetricks d3dcompiler_47` into the game's prefix, or copy one from another game) and re-run `--launch-options`.
- `PROTON_ENABLE_NVAPI=1` — only for Proton older than 9 (9+ has NVAPI on by default); harmless when redundant.

Steam edits are atomic and verified: the file is re-parsed after the edit and must be byte-identical apart from that one value, or nothing is written. Backups: `localconfig.vdf.dlss5o.orig` (first ever edit) and `.dlss5o.bak` (before each edit) next to the file, under `~/.local/share/Steam/userdata/<id>/config/`.

## GPU support

NVIDIA RTX only (the DLSS 5 model needs tensor cores and NGX). The `310.8.SF` build the tool installs adds patched binaries for RTX 40 and an FP16 path for RTX 20/30; RTX 50 runs the native FP8 kernels. The proprietary NVIDIA driver is required — including its Wine/NGX files (`nvngx.dll` under `/usr/lib/nvidia/wine` or your distro's equivalent; package `nvidia-utils` on Arch). Misdetected? `DLSS5ONECLICK_SKIP_GPU_CHECK=1` bypasses the refusal.

## Not handled

Same as upstream: DirectX 9 (except behind dgVoodoo2) and Vulkan-native games (X4, most native Linux ports have no Windows exe at all and are skipped), and games with anti-cheat (EAC/BattlEye/GameGuard — refused; `--ignore-anticheat` at your own risk, and under Proton that risk includes the anti-cheat's Linux path breaking outright). 32-bit DX11 games work since upstream 0.10.0 (Feeder + host64 helper, beta).

- The DLSS 5 add-on and its model are a leaked, closed-source build. The tool downloads whatever the rhi-repo releases currently host and cannot vouch for them.
- Heroic environment-variable writing is experimental (config format tolerances built in; falls back to showing you the variables).
- Lutris: games are listed and installable; add the shown variables in the game's Lutris settings yourself (v1 does not edit Lutris configs).

## Development

Rust 2021, single crate. GUI is egui/eframe; HTTP is reqwest (rustls); archives via the `zip` crate. The crate still compiles on Windows (all Linux integration is `cfg`-gated) so upstream changes merge cleanly.

```
cargo test                                  # no network, no Steam needed
cargo clippy --all-targets -- -D warnings
cargo build --release                       # target/release/dlss5oneclick
packaging/build-appimage.sh                 # optional AppImage
```

Verified 2026-09-02 on Arch (Hyprland/Wayland, RTX 4090, driver 610.57): library discovery against a real Steam install, `--check`/`--diagnose` against Cyberpunk 2077 (native DLSS, DX12, complete manual install detected and NR confirmed evaluating) and Skyrim SE (Feeder path plan), launch-option merging against real hand-written `localconfig.vdf` entries (idempotent), GUI on Wayland.

## Credits

This tool only automates other people's work — see [upstream's credits](https://github.com/faisalkindi/DLSS5oneclick#credits) for the full list: crosire (ReShade), jlrouzies-fr (DLSS5-Feeder), Afzaal/Kaidō (LumeniteFX), clshortfuse & the RenoDX community, RankFTW (rhi-repo), NVIDIA, DSOGaming, Dagherbou (OptiScaler_DLSSNR) and the OptiScaler team, NIGos (dlss5-bridge), emilk (egui). And **[faisalkindi](https://github.com/faisalkindi)** for DLSS5oneclick itself, which this port builds on.

## License

MIT for this tool (as upstream). Each downloaded component keeps its own license; the DLSS 5 add-on (`renodx-dlss5.addon64`) is closed source with no license published, and the NVIDIA runtimes are under NVIDIA's terms.
