# DLSS5oneclick

One button that sets up **DLSS 5 neural rendering** in a DirectX 11/12 game that has no DLSS of its own.

It does, in order, exactly what the [DLSS5-Feeder README](https://github.com/jlrouzies-fr/DLSS5-Feeder#install-for-a-64-bit-game) tells you to do by hand:

| Step | What | From |
|---|---|---|
| 1 | ReShade **with add-on support**, dropped as `dxgi.dll` | `ReShade_Setup_<ver>_Addon.exe` on [reshade.me](https://reshade.me) (DLL pulled straight out of the installer, nothing is run) |
| 2 | `dlss5-feed.addon64` + `DLSS5_Feed.fx` | [jlrouzies-fr/DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder/releases/latest) |
| 3 | Motion-vector provider: `lumenite_*.fx`, `include\*.fxh`, `lumenite_bluenoise256.png` | [umar-afzaal/LumeniteFX](https://github.com/umar-afzaal/LumeniteFX) (`mainline` branch) |
| 4 | `renodx-dlss5.addon64`, `nvngx_dlssnr.dll`, `nvngx_dlss.dll` | [RankFTW/rhi-repo](https://github.com/RankFTW/rhi-repo/releases) releases (`renodx-dlss5-*`, `dlssnr-*`, `dlss-*`) |
| 5 | `ReShade.ini` gets `PreprocessorDefinitions=DLSS5_MV_PROVIDER=3`; `ReShadePreset.ini` enables `Lumenite_Kernel` **above** `DLSS5_Feed` | written by this tool, existing keys preserved |

Every file is downloaded fresh from its upstream at install time. Nothing third-party is bundled.

## Use

1. Run `dlss5oneclick.exe` (single native binary, no runtime needed).
2. Browse to the game's `.exe`. The list shows what is already present.
3. **Install DLSS 5**.
4. In game: **Home** opens ReShade. In the **DLSS 5 Neural Rendering** panel, enable it. Keep the game's MSAA/SSAA off.

`dlss5-feed.log` next to the game exe should show `feature ready … DLAA` and `DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> Lumenite_Kernel (enabled)`.

CLI: `dlss5oneclick.exe "C:\Games\Foo\Foo.exe"` / `--remove` (headless, prints progress).

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

## License

MIT for this tool. Each downloaded component keeps its own license (ReShade: BSD-3; DLSS5-Feeder: see its repo; LumeniteFX: AGNYA; RenoDX: MIT; NVIDIA DLLs: NVIDIA's terms).
