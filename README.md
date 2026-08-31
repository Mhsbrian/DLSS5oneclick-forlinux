# DLSS5oneclick

One button that sets up the **leaked DLSS 5 neural-rendering build** in any DirectX 11/12 game, with or without DLSS of its own. Single native Windows exe, no runtime. Everything it installs is downloaded from the projects that made it; the only third-party content inside the exe is three SIL-OFL fonts.

Download: [latest release](https://github.com/faisalkindi/DLSS5oneclick/releases/latest) → `dlss5oneclick.exe`.

## Two paths, picked automatically

| The game | What gets installed |
|---|---|
| **Ships its own DLSS** (an `nvngx_dlss.dll` this tool did not place, or Streamline `sl.*.dll`, `nvngx_dlssg/dlssd.dll`, anywhere up to four folders deep) | ReShade add-on build + the DLSS 5 add-on (`renodx-dlss5.addon64`, `nvngx_dlssnr.dll`). The add-on hooks the game's own NGX calls directly. **DX11 games** also get [dlss5-dx11-bridge](https://github.com/NIGos/dlss5-dx11-bridge), which replays the D3D11 DLSS calls on a private D3D12 device so the add-on can see them. No Feeder, no LumeniteFX; a Feeder left over from an earlier run is removed. |
| **Has no DLSS** | The full Feeder path below: ReShade + shader headers + DLSS5-Feeder + LumeniteFX + the DLSS 5 add-on + config. |

**Engine choice for games with native DLSS**: the default engine is ReShade + the RenoDX add-on. A second engine — [Dagherbou's OptiScaler_DLSSNR fork](https://github.com/Dagherbou/OptiScaler_DLSSNR) (OptiScaler with a built-in Neural Rendering pass, colour composition from RenoDX under MIT) — can be picked in the GUI or with `--engine=opti`: the tool extracts the fork's release into the game as `dxgi.dll`, adds `nvngx_dlssnr.dll`, and records a manifest so Remove takes it out cleanly. In game, Insert opens the OptiScaler overlay; Neural Rendering is off by default there. The two engines cannot share a game (both load as dxgi.dll). Note the fork targets the unpatched model: on the driver's own DLL that means RTX 50; with the `310.8.SF` model this tool installs, older RTX generations may work but are untested there.

DX11 vs DX12 is read from the exe's import table, then from the engine DLLs next to it (`UnityPlayer.dll`, ...). When neither says, DX12 is assumed and the status line says so. `dlss5oneclick.exe "<game folder>" --check` prints the detected mode, API and plan without installing anything.

### The no-DLSS path

It does, in order, exactly what the [DLSS5-Feeder README](https://github.com/jlrouzies-fr/DLSS5-Feeder#install-for-a-64-bit-game) tells you to do by hand:

| Step | What | From |
|---|---|---|
| 1 | ReShade **with add-on support**, dropped as `dxgi.dll` | `ReShade_Setup_<ver>_Addon.exe` on [reshade.me](https://reshade.me) (DLL pulled straight out of the installer, nothing is run) |
| 2 | `ReShade.fxh`, `ReShadeUI.fxh`, `DrawText.fxh` into `reshade-shaders\Shaders` (the setup exe has only the DLLs; every shader below includes `ReShade.fxh`) | [crosire/reshade-shaders](https://github.com/crosire/reshade-shaders/tree/slim/Shaders) (`slim` branch) |
| 3 | `dlss5-feed.addon64` + `DLSS5_Feed.fx` | [jlrouzies-fr/DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder/releases/latest) |
| 4 | Motion-vector provider: `lumenite_*.fx`, `include\*.fxh`, `lumenite_bluenoise256.png` | [umar-afzaal/LumeniteFX](https://github.com/umar-afzaal/LumeniteFX) (`mainline` branch) |
| 5 | `renodx-dlss5.addon64` (the leaked DLSS 5 add-on, closed-source and community-distributed), `nvngx_dlssnr.dll` (its neural-rendering model), `nvngx_dlss.dll` (DLSS runtime; the Feeder's NGX session fails without one next to the game, so it is always placed and marked with a `nvngx_dlss.dll.dlss5oneclick` sidecar) | [RankFTW/rhi-repo](https://github.com/RankFTW/rhi-repo/releases) releases (`renodx-dlss5-*`, `dlssnr-*`, `dlss-*`) |
| 6 | `ReShade.ini` gets `PreprocessorDefinitions=DLSS5_MV_PROVIDER=3`; `ReShadePreset.ini` enables `Lumenite_Kernel` **above** `DLSS5_Feed` | written by this tool, existing keys preserved |

Every file is downloaded from its upstream at install time; a re-run only fetches what is missing.

## Use

1. Run `dlss5oneclick.exe` (single native binary, no runtime needed).
2. Pick the game's **folder** (or its `.exe`) - the game exe is detected automatically (the folder and two levels below it are searched, so `bin\x64_dx12\` and Unreal `Binaries\Win64\` layouts work; Unity crash handlers, Unreal helpers and redist installers are skipped; a `*-Shipping.exe` is preferred). If several candidates remain, a dropdown lets you choose. The list shows what is already present.
3. **Install DLSS 5**.
4. In game: **Home** opens ReShade. In the **DLSS 5 Neural Rendering** panel, enable it. Keep the game's MSAA/SSAA off.

**F6** toggles neural rendering on/off, **F5** saves the add-on's screenshot (both are the add-on's own hotkeys). On the Feeder path, `dlss5-feed.log` next to the game exe should show `feature ready … DLAA` and `DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> Lumenite_Kernel (enabled)`.

CLI: `dlss5oneclick.exe "C:\Games\Foo"` (folder or exe) / `--check` (detect only) / `--remove` (headless, prints progress).

## Known issues

- **Feeder path + exclusive fullscreen.** Every focus change (alt-tab) makes the game recreate its swapchain; DLSS5-Feeder rebuilds its DLSS feature and can crash inside `CreateFeature` on that rebuild ([Feeder issue #16](https://github.com/jlrouzies-fr/DLSS5-Feeder/issues/16), upstream). The game keeps rendering, DLSS 5 stops. Use borderless/windowed; raising `create_delay` in `dlss5-feed.cfg` helps. Seen on Fell & Sell; the same game ran 16,000+ frames without a crash in borderless.
- **Frame cost.** Neural rendering at native 4K adds several milliseconds. With v-sync on at 60 Hz that shows up as a hard drop to 30 fps. Turn v-sync off, or lower `work_resolution` in `dlss5-feed.cfg` (Feeder path, D3D11 games).
- **API detection can come back unknown** (monolithic Unreal exes load D3D at runtime, nothing static to read). The tool then assumes DX12 and says so; a DX11 game in that state would miss the bridge. `--check` shows what was detected.
- The DLSS 5 add-on and its model are a leaked, closed-source build. The tool downloads whatever the rhi-repo releases currently host and cannot vouch for them.

## Not handled

- **32-bit games** — need the `host64` helper setup (see Feeder README); the tool refuses rather than half-install.
- **DirectX 9** and **Vulkan** games — different proxy / a Vulkan layer; refused.
- Online games — ReShade with add-ons trips anti-cheat.

## Development

Rust 2021, single crate. GUI is egui/eframe; HTTP is reqwest (rustls); archives via the `zip` crate.

```
cargo test
cargo build --release   # target/release/dlss5oneclick.exe
```

Tests use local fakes only; no network. Verified 2026-08-31: full live installs against dummy game folders (both paths), and detection against real installs — Fell & Sell (Unity, DX11, no DLSS → Feeder), Fatal Claw (Unreal, DX11 + DLSS → native + bridge), Mortal Shell 2 (Unreal + DLSS → native), The Witcher 3 (`bin\x64_dx12`, native DX12), Jotunnslayer and Trails in the Sky (DX11 + DLSS → native + bridge). DLSS 5 confirmed running in Fell & Sell (`feature ready … DLAA`, NR evaluating, F6 toggling).

## Credits

This tool only automates other people's work. The credit belongs to:

- **[crosire](https://github.com/crosire)** — [ReShade](https://reshade.me) and [reshade-shaders](https://github.com/crosire/reshade-shaders), the injection framework everything here runs inside.
- **[jlrouzies-fr](https://github.com/jlrouzies-fr)** — [DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder), the add-on that builds a DLSS contract from ReShade depth + motion vectors, and the install guide this tool follows step by step.
- **[Afzaal (Kaidō)](https://github.com/umar-afzaal)** — [LumeniteFX](https://github.com/umar-afzaal/LumeniteFX), the motion-vector provider (Kernel 2.0).
- **[clshortfuse](https://github.com/clshortfuse)** and the RenoDX community — [RenoDX](https://github.com/clshortfuse/renodx), which the DLSS 5 neural-rendering add-on is built on.
- **[RankFTW](https://github.com/RankFTW)** — [RHI](https://github.com/RankFTW/RHI) and the [rhi-repo](https://github.com/RankFTW/rhi-repo) releases that host the DLSS 5 add-on and the NVIDIA runtimes.
- **NVIDIA** — DLSS 5 itself and the `nvngx_dlssnr.dll` / `nvngx_dlss.dll` runtimes.
- **DSOGaming** — the [article](https://www.dsogaming.com/articles/heres-how-you-can-install-dlss-5-to-all-dx9-dx10-dx11-dx12-and-vulkan-games/) that put the pieces together and started this.
- **[Dagherbou](https://github.com/Dagherbou)** — [OptiScaler_DLSSNR](https://github.com/Dagherbou/OptiScaler_DLSSNR), the OptiScaler fork with the built-in Neural Rendering pass, and the **[OptiScaler team](https://github.com/optiscaler/OptiScaler)** it builds on (GPL-3).
- **[NIGos](https://github.com/NIGos)** — [dlss5-dx11-bridge](https://github.com/NIGos/dlss5-dx11-bridge), which lets the DLSS 5 add-on work in D3D11 games that have their own DLSS.
- **[emilk](https://github.com/emilk)** — [egui / eframe](https://github.com/emilk/egui), the UI toolkit.
- Fonts: [Sora](https://github.com/sora-xor/sora-font) by the Sora project, [IBM Plex Sans](https://github.com/IBM/plex) by IBM, [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) by JetBrains — all SIL OFL.

## License

MIT for this tool. Each downloaded component keeps its own license: ReShade BSD-3; DLSS5-Feeder — see its repo; LumeniteFX — AGNYA; dlss5-dx11-bridge MIT; the DLSS 5 add-on (`renodx-dlss5.addon64`) — closed source, no license published; NVIDIA runtimes — NVIDIA's terms.
