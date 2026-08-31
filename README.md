# DLSS5oneclick

One button that sets up the **leaked DLSS 5 neural-rendering build** in a DirectX 11/12 game that has no DLSS of its own. Single native Windows exe, no runtime.

Download: [latest release](https://github.com/faisalkindi/DLSS5oneclick/releases/latest) → `dlss5oneclick.exe`.

It does, in order, exactly what the [DLSS5-Feeder README](https://github.com/jlrouzies-fr/DLSS5-Feeder#install-for-a-64-bit-game) tells you to do by hand:

| Step | What | From |
|---|---|---|
| 1 | ReShade **with add-on support**, dropped as `dxgi.dll` | `ReShade_Setup_<ver>_Addon.exe` on [reshade.me](https://reshade.me) (DLL pulled straight out of the installer, nothing is run) |
| 1b | `ReShade.fxh`, `ReShadeUI.fxh`, `DrawText.fxh` into `reshade-shaders\Shaders` (the setup exe has only the DLLs; every shader below includes `ReShade.fxh`) | [crosire/reshade-shaders](https://github.com/crosire/reshade-shaders/tree/slim/Shaders) (`slim` branch) |
| 2 | `dlss5-feed.addon64` + `DLSS5_Feed.fx` | [jlrouzies-fr/DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder/releases/latest) |
| 3 | Motion-vector provider: `lumenite_*.fx`, `include\*.fxh`, `lumenite_bluenoise256.png` | [umar-afzaal/LumeniteFX](https://github.com/umar-afzaal/LumeniteFX) (`mainline` branch) |
| 4 | `renodx-dlss5.addon64` (the leaked DLSS 5 add-on), `nvngx_dlssnr.dll`, `nvngx_dlss.dll` | [RankFTW/rhi-repo](https://github.com/RankFTW/rhi-repo/releases) releases (`renodx-dlss5-*`, `dlssnr-*`, `dlss-*`) |
| 5 | `ReShade.ini` gets `PreprocessorDefinitions=DLSS5_MV_PROVIDER=3`; `ReShadePreset.ini` enables `Lumenite_Kernel` **above** `DLSS5_Feed` | written by this tool, existing keys preserved |

Every file is downloaded fresh from its upstream at install time. Nothing third-party is bundled.

## Use

1. Run `dlss5oneclick.exe` (single native binary, no runtime needed).
2. Pick the game's **folder** (or its `.exe`) - the game exe is detected automatically (Unity crash handlers, Unreal helpers and redist installers are skipped; Unreal `Binaries\Win64\*-Shipping.exe` is preferred). If several candidates remain, a dropdown lets you choose. The list shows what is already present.
3. **Install DLSS 5**.
4. In game: **Home** opens ReShade. In the **DLSS 5 Neural Rendering** panel, enable it. Keep the game's MSAA/SSAA off.

`dlss5-feed.log` next to the game exe should show `feature ready … DLAA` and `DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> Lumenite_Kernel (enabled)`.

CLI: `dlss5oneclick.exe "C:\Games\Foo"` (folder or exe) / `--remove` (headless, prints progress).

## Not handled

- **32-bit games** — need the `host64` helper setup (see Feeder README); the tool refuses rather than half-install.
- **DirectX 9** and **Vulkan** games — different proxy / a Vulkan layer; refused.
- Games that already have DLSS — use [dlss5-dx11-bridge](https://github.com/NIGos/dlss5-dx11-bridge) instead, not this.
- Online games — ReShade with add-ons trips anti-cheat.

## Development

Rust 2021, single crate. GUI is egui/eframe; HTTP is reqwest (rustls); archives via the `zip` crate.

```
cargo test
cargo build --release   # target/release/dlss5oneclick.exe
```

Tests use local fakes only; no network. Verified end-to-end against a dummy 64-bit exe 2026-08-31.

## Credits

This tool only automates other people's work. The credit belongs to:

- **[crosire](https://github.com/crosire)** — [ReShade](https://reshade.me) and [reshade-shaders](https://github.com/crosire/reshade-shaders), the injection framework everything here runs inside.
- **[jlrouzies-fr](https://github.com/jlrouzies-fr)** — [DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder), the add-on that builds a DLSS contract from ReShade depth + motion vectors, and the install guide this tool follows step by step.
- **[Afzaal (Kaidō)](https://github.com/umar-afzaal)** — [LumeniteFX](https://github.com/umar-afzaal/LumeniteFX), the motion-vector provider (Kernel 2.0).
- **[clshortfuse](https://github.com/clshortfuse)** and the RenoDX community — [RenoDX](https://github.com/clshortfuse/renodx), which the DLSS 5 neural-rendering add-on is built on.
- **[RankFTW](https://github.com/RankFTW)** — [RHI](https://github.com/RankFTW/RHI) and the [rhi-repo](https://github.com/RankFTW/rhi-repo) releases that host the DLSS 5 add-on and the NVIDIA runtimes.
- **NVIDIA** — DLSS 5 itself and the `nvngx_dlssnr.dll` / `nvngx_dlss.dll` runtimes.
- **DSOGaming** — the [article](https://www.dsogaming.com/articles/heres-how-you-can-install-dlss-5-to-all-dx9-dx10-dx11-dx12-and-vulkan-games/) that put the pieces together and started this.
- **[emilk](https://github.com/emilk)** — [egui / eframe](https://github.com/emilk/egui), the UI toolkit.
- Fonts: [Sora](https://github.com/sora-xor/sora-font) by the Sora project, [IBM Plex Sans](https://github.com/IBM/plex) by IBM, [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) by JetBrains — all SIL OFL.

## License

MIT for this tool. Each downloaded component keeps its own license (ReShade: BSD-3; DLSS5-Feeder: see its repo; LumeniteFX: AGNYA; RenoDX: MIT; NVIDIA DLLs: NVIDIA's terms).
