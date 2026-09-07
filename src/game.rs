//! Game-folder inspection: exe bitness, ReShade presence, installed pieces.

use crate::gpu;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const PE_X64: u16 = 0x8664;
pub const PE_X86: u16 = 0x014C;

pub const FEEDER_ADDON: &str = "dlss5-feed.addon64";
/// 32-bit games: the in-game half of the Feeder, and the 64-bit helper folder
/// beside the exe that holds everything a 32-bit process cannot load
/// (a 64-bit ReShade, the DLSS 5 add-on, the two NVIDIA DLLs, the helper exe).
pub const FEEDER_ADDON32: &str = "dlss5-feed.addon32";
pub const HOST_DIR: &str = "host64";
pub const HOST_EXE: &str = "dlss5-feed-host64.exe";
pub const FEEDER_FX: &str = "DLSS5_Feed.fx";
pub const DLSS5_ADDON: &str = "renodx-dlss5.addon64";
pub const DLSSNR_DLL: &str = "nvngx_dlssnr.dll";
pub const DLSS_DLL: &str = "nvngx_dlss.dll";
pub const LUMENITE_KERNEL_FX: &str = "lumenite_Kernel.fx";
pub const LUMENITE_BLUENOISE: &str = "lumenite_bluenoise256.png";
pub const BRIDGE_ADDON: &str = "dlss5-bridge.addon64";
/// matiasLombo's neural-upstream add-on. The name is not ours to choose: the
/// NGX snippet gates feature creation on the calling module's path containing
/// `nvngx.dll`, and under any other name it returns 0xBAD00002 and does nothing.
pub const UPSTREAM_ADDON: &str = "nvngx.dll.addon64";
/// Files this tool wrote for an OptiScaler install, one path per line.
pub const OPTI_MANIFEST: &str = ".dlss5oneclick-optiscaler-manifest";
/// Sidecar written next to an `nvngx_dlss.dll` this tool placed, so it is never mistaken for the game's own.
pub const DLSS_MARKER: &str = "nvngx_dlss.dll.dlss5oneclick";
/// Same idea for the neural model and for ReShade: the release tag / version this
/// tool placed, so Install can tell "ours and stale" from "the user's own".
pub const DLSSNR_MARKER: &str = "nvngx_dlssnr.dll.dlss5oneclick";
pub const RESHADE_MARKER: &str = "dxgi.dll.dlss5oneclick";
/// The DLSS5-Feeder release tag this tool placed, so a stale install can be
/// spotted without downloading the zip to compare sizes.
pub const FEEDER_MARKER: &str = "dlss5-feed.dlss5oneclick";
pub const RESHADE_PROXY: &str = "dxgi.dll";
/// Shader headers the official installer fetches from crosire/reshade-shaders (branch `slim`).
/// Not inside the setup exe. DLSS5_Feed.fx and every lumenite_*.fx include ReShade.fxh;
/// DLSS5_Feed.fx also includes DrawText.fxh; ReShadeUI.fxh is the standard companion.
pub const RESHADE_HEADERS: [&str; 3] = ["ReShade.fxh", "ReShadeUI.fxh", "DrawText.fxh"];
/// Name of the RenoDX game mod this tool placed (one line), so Remove takes only that one.
pub const RENODX_MANIFEST: &str = ".dlss5oneclick-renodx";
/// REFramework (praydog) loads as dinput8.dll; RE Engine games crash under ReShade without it.
pub const REFRAMEWORK_DLL: &str = "dinput8.dll";
pub const REFRAMEWORK_MARKER: &str = ".dlss5oneclick-reframework";
/// Every RE Engine game keeps its base archive under this name next to the exe.
pub const RE_ENGINE_PAK: &str = "re_chunk_000.pak";
pub const RESHADE_INI: &str = "ReShade.ini";
pub const RESHADE_PRESET: &str = "ReShadePreset.ini";

/// The entry in `dir` matching `name` case-insensitively; the on-disk casing wins.
/// Game folders sit on case-sensitive filesystems on Linux, and other tools (or a
/// Windows-made copy) may have created differently-cased versions of what this tool
/// looks for — an exact-case miss must not split the tree or hide an install.
pub fn existing_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.exists() {
        return Some(exact);
    }
    for e in fs::read_dir(dir).ok()?.flatten() {
        if e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            return Some(e.path());
        }
    }
    None
}

/// `dir/parts…` where each component reuses an existing entry's casing when one
/// matches case-insensitively, and keeps the given canonical casing otherwise.
pub fn join_ci(dir: &Path, parts: &[&str]) -> PathBuf {
    let mut p = dir.to_path_buf();
    for part in parts {
        p = existing_ci(&p, part).unwrap_or_else(|| p.join(part));
    }
    p
}

fn file_ci(dir: &Path, name: &str) -> bool {
    existing_ci(dir, name).is_some_and(|p| p.is_file())
}

/// 64 or 32, read from the PE header's Machine field.
pub fn exe_bitness(exe: &Path) -> Result<u8> {
    let mut f = fs::File::open(exe).with_context(|| format!("cannot open {}", exe.display()))?;
    let mut head = [0u8; 0x40];
    f.read_exact(&mut head)
        .with_context(|| format!("{} is not a Windows executable", exe.display()))?;
    if &head[..2] != b"MZ" {
        bail!("{} is not a Windows executable", exe.display());
    }
    let pe_off = u32::from_le_bytes([head[0x3C], head[0x3D], head[0x3E], head[0x3F]]);
    f.seek(SeekFrom::Start(pe_off as u64))?;
    let mut sig = [0u8; 6];
    f.read_exact(&mut sig)
        .with_context(|| format!("{} has no PE header", exe.display()))?;
    if &sig[..4] != b"PE\0\0" {
        bail!("{} has no PE header", exe.display());
    }
    match u16::from_le_bytes([sig[4], sig[5]]) {
        PE_X64 => Ok(64),
        PE_X86 => Ok(32),
        m => bail!("{}: unsupported machine type 0x{m:04x}", exe.display()),
    }
}

/// A ReShade proxy DLL carries a literal "ReShade" string and is >1 MB.
pub fn is_reshade_dll(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() < (1 << 20) {
        return false;
    }
    match fs::read(path) {
        Ok(bytes) => is_reshade_image(&bytes),
        Err(_) => false,
    }
}

/// OptiScaler's own `dxgi.dll` carries the string `ReShade` six times, because
/// it can load ReShade itself — so "contains ReShade" called it ReShade and
/// refused to update a game that had OptiScaler installed. crosire's name is in
/// every real ReShade build (36 hits of `ReShade`, 3 of `crosire` in 6.8.0)
/// and in none of OptiScaler's, so it settles the two apart; a build that
/// somehow lacks it still counts unless the file says OptiScaler.
fn is_reshade_image(bytes: &[u8]) -> bool {
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    has(b"ReShade") && (has(b"crosire") || !has(b"OptiScaler"))
}

/// Which install path applies to a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Game has no DLSS: DLSS5-Feeder + LumeniteFX fake the DLSS contract.
    Feeder,
    /// Game ships its own DLSS: the DLSS 5 add-on hooks the game's NGX calls directly
    /// (plus dlss5-dx11-bridge when the game renders with D3D11).
    Native,
}

/// Graphics API the exe imports, from its PE import table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// Direct3D 10/10.1. The 32-bit Feeder add-on runs these natively from
    /// 0.13.1-beta.1 (a private D3D11 relay device inside the game process);
    /// a game imports `d3d10_1.dll` rather than `d3d10.dll` in practice.
    Dx10,
    Dx11,
    Dx12,
    /// Imports `d3d9.dll` and no newer Direct3D. DLSS needs a D3D11/12 device,
    /// so these need dgVoodoo2 in front before anything here applies -- and the
    /// system d3d9.dll is loaded by name, so a local ReShade d3d9.dll is what
    /// hooks it, never the dxgi.dll this tool installs (#16, Aion).
    Dx9,
    /// Imports `vulkan-1.dll` and no Direct3D. ReShade reaches a Vulkan game
    /// through a registered Vulkan layer, not through a `dxgi.dll` beside the
    /// exe, so this install has nothing to load (#6, Detroit: Become Human).
    Vulkan,
    /// Neither d3d11.dll nor d3d12.dll is a static import (loaded at runtime, or DX9/Vulkan).
    Unknown,
}

impl Api {
    pub fn label(self) -> &'static str {
        match self {
            Api::Dx10 => "DX10",
            Api::Dx11 => "DX11",
            Api::Dx12 => "DX12",
            Api::Dx9 => "DX9",
            Api::Vulkan => "Vulkan",
            Api::Unknown => "API unknown, assuming DX12",
        }
    }
}

/// Lower-cased DLL names from the exe's static import table. Empty on any parse problem.
/// Every function name the PE imports from `dll` (lowercased).
///
/// Needed because importing `d3d9.dll` does not make a game a Direct3D 9 game:
/// engines from the D3D9 era onward import it for the `D3DPERF_*` debug markers
/// alone and render with something far newer, which is how RDR2 read as
/// DirectX 9 (#53). A real D3D9 renderer calls `Direct3DCreate9`.
pub fn pe_import_fns(exe: &Path, dll: &str) -> Vec<String> {
    let Ok(data) = fs::read(exe) else {
        return vec![];
    };
    let rd32 = |o: usize| -> Option<u32> {
        data.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let rd64 = |o: usize| -> Option<u64> {
        data.get(o..o + 8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    };
    let rd16 =
        |o: usize| -> Option<u16> { data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) };
    let cstr = |off: usize| -> String {
        let end = data[off..]
            .iter()
            .position(|&b| b == 0)
            .map(|n| off + n)
            .unwrap_or(off);
        String::from_utf8_lossy(&data[off..end]).to_ascii_lowercase()
    };
    let parse = || -> Option<Vec<String>> {
        if data.get(..2)? != b"MZ" {
            return None;
        }
        let pe = rd32(0x3C)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = pe + 4;
        let nsec = rd16(coff + 2)? as usize;
        let opt_size = rd16(coff + 16)? as usize;
        let opt = coff + 20;
        let magic = rd16(opt)?;
        let (dd_off, pe32_plus) = match magic {
            0x20B => (112, true),
            0x10B => (96, false),
            _ => return None,
        };
        let import_rva = rd32(opt + dd_off + 8)? as usize;
        if import_rva == 0 {
            return Some(vec![]);
        }
        let sec = opt + opt_size;
        let mut sections = Vec::new();
        for i in 0..nsec {
            let s = sec + i * 40;
            sections.push((
                rd32(s + 12)? as usize,
                rd32(s + 16)? as usize,
                rd32(s + 20)? as usize,
            ));
        }
        let to_off = |rva: usize| -> Option<usize> {
            sections
                .iter()
                .find(|(va, size, _)| rva >= *va && rva < va + size)
                .map(|(va, _, raw)| raw + (rva - va))
        };
        let want = dll.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut desc = to_off(import_rva)?;
        for _ in 0..512 {
            let name_rva = rd32(desc + 12)? as usize;
            if name_rva == 0 && rd32(desc)? == 0 {
                break;
            }
            let is_ours = to_off(name_rva).map(&cstr).is_some_and(|n| n == want);
            if is_ours {
                // OriginalFirstThunk when present, else FirstThunk.
                let thunk_rva = match rd32(desc)? {
                    0 => rd32(desc + 16)? as usize,
                    v => v as usize,
                };
                if let Some(mut t) = to_off(thunk_rva) {
                    for _ in 0..4096 {
                        let (entry, step) = if pe32_plus {
                            (rd64(t)?, 8)
                        } else {
                            (rd32(t)? as u64, 4)
                        };
                        if entry == 0 {
                            break;
                        }
                        let by_ordinal = if pe32_plus {
                            entry & (1 << 63) != 0
                        } else {
                            entry & (1 << 31) != 0
                        };
                        if !by_ordinal {
                            // IMAGE_IMPORT_BY_NAME: 2-byte hint, then the name.
                            if let Some(off) = to_off(entry as usize & 0x7fff_ffff) {
                                out.push(cstr(off + 2));
                            }
                        }
                        t += step;
                    }
                }
            }
            desc += 20;
        }
        Some(out)
    };
    parse().unwrap_or_default()
}

pub fn pe_imports(exe: &Path) -> Vec<String> {
    let Ok(data) = fs::read(exe) else {
        return vec![];
    };
    let rd32 = |o: usize| -> Option<u32> {
        data.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let rd16 =
        |o: usize| -> Option<u16> { data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) };
    let parse = || -> Option<Vec<String>> {
        if data.get(..2)? != b"MZ" {
            return None;
        }
        let pe = rd32(0x3C)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = pe + 4;
        let nsec = rd16(coff + 2)? as usize;
        let opt_size = rd16(coff + 16)? as usize;
        let opt = coff + 20;
        let magic = rd16(opt)?;
        let dd_off = match magic {
            0x20B => 112,
            0x10B => 96,
            _ => return None,
        };
        let import_rva = rd32(opt + dd_off + 8)? as usize;
        if import_rva == 0 {
            return Some(vec![]);
        }
        let sec = opt + opt_size;
        let mut sections = Vec::new();
        for i in 0..nsec {
            let s = sec + i * 40;
            sections.push((
                rd32(s + 12)? as usize,
                rd32(s + 16)? as usize,
                rd32(s + 20)? as usize,
            ));
        }
        let to_off = |rva: usize| -> Option<usize> {
            sections
                .iter()
                .find(|(va, size, _)| rva >= *va && rva < va + size)
                .map(|(va, _, raw)| raw + (rva - va))
        };
        let mut names = Vec::new();
        let mut desc = to_off(import_rva)?;
        for _ in 0..512 {
            let name_rva = rd32(desc + 12)? as usize;
            if name_rva == 0 && rd32(desc)? == 0 {
                break;
            }
            if let Some(off) = to_off(name_rva) {
                let end = data[off..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|n| off + n)
                    .unwrap_or(off);
                names.push(String::from_utf8_lossy(&data[off..end]).to_ascii_lowercase());
            }
            desc += 20;
        }
        Some(names)
    };
    parse().unwrap_or_default()
}

/// Which D3D a static import table implies (also used by the exe finder to
/// prefer a provably-D3D sibling). `d3d10_1.dll` is what a Direct3D 10 game
/// actually imports; the upstream installer looked only for `d3d10.dll` and
/// mistook such games for DirectX 9.
pub fn classify_imports(imports: &[String]) -> Api {
    let has = |n: &str| imports.iter().any(|i| i == n);
    if has("d3d12.dll") {
        Api::Dx12
    } else if has("d3d11.dll") {
        Api::Dx11
    } else if has("d3d10_1.dll") || has("d3d10.dll") {
        Api::Dx10
    } else if has("vulkan-1.dll") {
        Api::Vulkan
    } else if has("d3d9.dll") {
        Api::Dx9
    } else {
        Api::Unknown
    }
}

/// True when the game carries its own DirectX 12 Agility SDK runtime.
/// Unreal puts it in a `D3D12` subfolder; Unity players declare the exe's own
/// folder, so the file sits directly beside the exe. Both count, and the
/// second layout is the one that produced a 0x887E0003 nobody could find
/// (dlss5-bridge#24).
pub fn has_agility_redist(dir: &Path) -> Option<PathBuf> {
    [
        dir.join("D3D12").join("D3D12Core.dll"),
        dir.join("D3D12Core.dll"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// An import that only a renderer newer than Direct3D 9 would carry.
///
/// Direct3D 9 predates DXGI and never uses it, so `dxgi.dll` settles it alone.
/// Beyond that, upscaler and latency libraries are shipped per graphics API and
/// name it in the file: FidelityFX FSR2's backends are `..._dx12_x64.dll` and
/// `..._vk_x64.dll` (AMD ships one per target API), and NVIDIA's Reflex library
/// for Vulkan is `nvlowlatencyvk.dll`. Red Dead Redemption 2 imports all three
/// while creating its real device at runtime, and statically imports
/// `Direct3DCreate9Ex` besides, so neither the marker rule nor DXGI alone
/// caught it (#53).
fn implies_newer_than_d3d9(import: &str) -> bool {
    let stem = import.strip_suffix(".dll").unwrap_or(import);
    matches!(
        stem,
        "dxgi" | "d3d10" | "d3d10_1" | "d3d11" | "d3d12" | "vulkan-1"
    ) || stem.contains("dx11")
        || stem.contains("dx12")
        || stem.contains("vulkan")
        || stem.ends_with("vk")
        || stem.contains("_vk_")
}

/// True when the PE imports `d3d9.dll` for something other than the
/// `D3DPERF_*` debug markers *and* carries nothing that implies a newer API.
fn d3d9_is_the_renderer(pe: &Path) -> bool {
    let imports = pe_imports(pe);
    if imports.iter().any(|i| implies_newer_than_d3d9(i)) {
        return false;
    }
    let fns = pe_import_fns(pe, "d3d9.dll");
    fns.is_empty() || fns.iter().any(|f| !f.starts_with("d3dperf_"))
}

pub fn detect_api(exe: &Path) -> Api {
    use classify_imports as classify;
    let mut api = classify(&pe_imports(exe));
    if api == Api::Dx9 && !d3d9_is_the_renderer(exe) {
        api = Api::Unknown;
    }
    let agility_sdk = exe
        .parent()
        .is_some_and(|d| has_agility_redist(d).is_some());
    if api == Api::Dx12 || (api == Api::Dx11 && agility_sdk) {
        // The DirectX 12 Agility SDK redist ships only with D3D12 renderers;
        // RE Engine exes import d3d11.dll statically and create D3D12 at runtime.
        return Api::Dx12;
    }
    if api != Api::Unknown {
        return api;
    }
    // Engines like Unity and Unreal load D3D from an engine DLL next to the exe
    // (UnityPlayer.dll, *-Win64-Shipping.dll, ...). Scan those, largest first,
    // skipping proxies/add-ons that would mislead (dxgi.dll, d3d*.dll, nvngx*).
    let Some(dir) = exe.parent() else {
        return Api::Unknown;
    };
    let Ok(rd) = fs::read_dir(dir) else {
        return Api::Unknown;
    };
    let mut dlls: Vec<(u64, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")))
        .filter(|p| {
            let n = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            !(n.starts_with("dxgi")
                || n.starts_with("d3d")
                || n.starts_with("nvngx")
                || n.starts_with("reshade"))
        })
        .filter_map(|p| fs::metadata(&p).ok().map(|m| (m.len(), p)))
        .collect();
    dlls.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
    let mut seen_dx11 = false;
    let mut seen_dx9 = false;
    for (_, dll) in dlls.into_iter().take(12) {
        match classify_imports(&pe_imports(&dll)) {
            Api::Dx12 => return Api::Dx12,
            Api::Dx11 => seen_dx11 = true,
            // Aion renders through XRenderD3D9.dll rather than from the exe,
            // so the renderer DLL is the only place the API shows (#16). The
            // marker-only case is excluded here too (#53).
            Api::Dx9 => seen_dx9 |= d3d9_is_the_renderer(&dll),
            Api::Dx10 | Api::Vulkan | Api::Unknown => {}
        }
    }
    if seen_dx9 && !seen_dx11 {
        return Api::Dx9;
    }
    if seen_dx11 {
        Api::Dx11
    } else {
        Api::Unknown
    }
}

/// dgVoodoo2 next to the exe: its own config/control panel, or its name inside
/// the wrapper DLL. It translates D3D9 to D3D11, which the Feeder supports.
pub fn is_dgvoodoo(game_dir: &Path) -> bool {
    if game_dir.join("dgVoodoo.conf").is_file() || game_dir.join("dgVoodooCpl.exe").is_file() {
        return true;
    }
    let dll = game_dir.join("d3d9.dll");
    match fs::read(&dll) {
        Ok(b) => b.windows(8).any(|w| w.eq_ignore_ascii_case(b"dgVoodoo")),
        Err(_) => false,
    }
}

/// Anti-cheat present in the install tree, by the files those systems ship.
/// ReShade add-on injection is exactly what they look for: kicks at best, bans
/// at worst. Verified file names: EAC `EasyAntiCheat[_EOS]/EasyAntiCheat_EOS_Setup.exe`,
/// BattlEye `BattlEye/BEService_x64.exe`, `Install_BattlEye.bat`, `*_BE.exe`,
/// GameGuard `tools/GGSetup.exe` or a `GameGuard` folder.
pub fn detect_anticheat(game_dir: &Path) -> Option<&'static str> {
    fn walk(d: &Path, depth: u8) -> Option<&'static str> {
        let rd = fs::read_dir(d).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            let n = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if p.is_dir() {
                if n == "easyanticheat" || n == "easyanticheat_eos" {
                    return Some("Easy Anti-Cheat");
                }
                if n == "battleye" {
                    return Some("BattlEye");
                }
                if n == "gameguard" {
                    return Some("GameGuard");
                }
                if depth > 0 {
                    if let Some(hit) = walk(&p, depth - 1) {
                        return Some(hit);
                    }
                }
            } else {
                if n.starts_with("easyanticheat") && n.ends_with(".exe") {
                    return Some("Easy Anti-Cheat");
                }
                if n == "beservice_x64.exe" || n == "install_battleye.bat" || n.ends_with("_be.exe")
                {
                    return Some("BattlEye");
                }
                if n == "ggsetup.exe" || n == "gameguard.des" {
                    return Some("GameGuard");
                }
            }
        }
        None
    }
    walk(game_dir, 3)
}

/// Online titles whose anti-cheat ships no marker files (Blizzard's, Riot's).
/// Overwatch also blocks unsigned DLLs outright (add-ons fail with 0x80090006).
pub fn known_anticheat_exe(exe: &Path) -> Option<&'static str> {
    let n = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match n.as_str() {
        "overwatch.exe" => Some("Blizzard anti-cheat (Overwatch)"),
        "valorant.exe" | "valorant-win64-shipping.exe" => Some("Riot Vanguard"),
        "leagueclient.exe" | "league of legends.exe" => Some("Riot Vanguard"),
        _ => None,
    }
}

/// True if the game ships its own DLSS.
///
/// Signals, any one is enough: an `nvngx_dlss.dll` under the exe's folder (depth <= 4)
/// that this tool did not place (no sidecar marker next to it), or Streamline /
/// frame-generation / ray-reconstruction runtimes (`sl.*.dll`, `nvngx_dlssg.dll`,
/// `nvngx_dlssd.dll`) which only a DLSS-integrated game ships.
pub fn game_ships_dlss(game_dir: &Path) -> bool {
    fn walk(d: &Path, depth: u8) -> bool {
        let Ok(rd) = fs::read_dir(d) else {
            return false;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                let n = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if n == DLSS_DLL && !p.with_file_name(DLSS_MARKER).is_file() {
                    return true;
                }
                if n == "nvngx_dlssg.dll"
                    || n == "nvngx_dlssd.dll"
                    || (n.starts_with("sl.") && n.ends_with(".dll"))
                {
                    return true;
                }
            } else if depth > 0 && p.is_dir() && walk(&p, depth - 1) {
                return true;
            }
        }
        false
    }
    if walk(game_dir, 4) {
        return true;
    }
    // Unreal: exe in <Project>/Binaries/Win64, DLSS in
    // <Project>/Plugins/NVIDIA/DLSS/Binaries/ThirdParty/Win64 (or Engine/Plugins/...).
    let unreal_root = game_dir
        .parent()
        .filter(|b| {
            b.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("binaries"))
        })
        .and_then(Path::parent);
    match unreal_root {
        Some(proj) => {
            walk(&proj.join("Plugins"), 7)
                || proj
                    .parent()
                    .is_some_and(|root| walk(&root.join("Engine").join("Plugins"), 7))
        }
        None => false,
    }
}

#[derive(Debug, Clone)]
pub struct GameStatus {
    pub mode: Mode,
    pub api: Api,
    pub bridge: bool,
    pub opti: bool,
    pub gpu: Option<(gpu::Gpu, gpu::Tier)>,
    pub exe: PathBuf,
    pub bitness: u8,
    pub reshade: bool,
    pub headers: bool,
    pub feeder: bool,
    pub lumenite: bool,
    pub dlss5_addon: bool,
    pub dlssnr: bool,
    pub dlss: bool,
    /// What the folder scan said, before any override.
    pub mode_detected: Mode,
    /// 32-bit only: `host64\dlss5-feed-host64.exe` and a 64-bit ReShade beside it.
    pub host_exe: bool,
    pub host_reshade: bool,
    /// Capcom RE Engine (needs REFramework before ReShade will run).
    pub re_engine: bool,
    pub reframework: bool,
    /// matiasLombo's neural-upstream add-on is in the folder.
    pub upstream: bool,
    /// RenoDX game mod this tool installed, from its manifest.
    pub renodx_mod: Option<String>,
    /// Other RenoDX game mods found in the folder (not ours, not the DLSS 5 add-on).
    pub foreign_renodx: Vec<String>,
    /// RTX 40 DLSS MFG unlock installed by this tool (its manifest is present).
    pub mfg: bool,
    /// Game ships Streamline DLSS Frame Generation (what MFG multiplies).
    pub has_fg: bool,
    /// Anti-cheat found (files or exe name), whether or not the refusal is overridden.
    pub anticheat: Option<&'static str>,
    pub problems: Vec<String>,
}

pub const IGNORE_ANTICHEAT_ENV: &str = "DLSS5ONECLICK_IGNORE_ANTICHEAT";
/// `feeder` or `native`: override the DLSS detection (a stray `nvngx_dlss.dll`
/// makes a game without DLSS look native; some games load DLSS from elsewhere).
pub const MODE_ENV: &str = "DLSS5ONECLICK_MODE";

pub fn mode_override() -> Option<Mode> {
    match std::env::var(MODE_ENV).ok()?.to_ascii_lowercase().as_str() {
        "feeder" | "nodlss" | "no-dlss" => Some(Mode::Feeder),
        "native" | "dlss" => Some(Mode::Native),
        _ => None,
    }
}

/// GUI dropdown / `--mode=`: same switch as the environment variable.
pub fn set_mode_override(m: Option<Mode>) {
    match m {
        Some(Mode::Feeder) => std::env::set_var(MODE_ENV, "feeder"),
        Some(Mode::Native) => std::env::set_var(MODE_ENV, "native"),
        None => std::env::remove_var(MODE_ENV),
    }
}

pub fn ignore_anticheat() -> bool {
    std::env::var_os(IGNORE_ANTICHEAT_ENV).is_some()
}

/// GUI checkbox / `--ignore-anticheat`: same switch as the environment variable.
pub fn set_ignore_anticheat(on: bool) {
    if on {
        std::env::set_var(IGNORE_ANTICHEAT_ENV, "1");
    } else {
        std::env::remove_var(IGNORE_ANTICHEAT_ENV);
    }
}

pub const BRIDGE_ENV: &str = "DLSS5ONECLICK_BRIDGE";

/// `--bridge` / `DLSS5ONECLICK_BRIDGE=1`: include the DX11 bridge for a
/// native-DLSS game even when the import table could not prove D3D11 (some
/// monolithic exes load D3D at runtime and read as "assume DX12").
pub fn bridge_override() -> bool {
    std::env::var(BRIDGE_ENV).is_ok_and(|v| v == "1")
}

impl GameStatus {
    pub fn game_dir(&self) -> &Path {
        self.exe.parent().expect("exe has a parent")
    }
    /// Why the ReShade engine cannot reach this game, when that is the case.
    /// A Vulkan game has no `dxgi.dll` to hook, so ReShade must be installed as
    /// a Vulkan layer by its own setup -- but OptiScaler reaches Vulkan games
    /// perfectly well, so this refuses one engine, not the game (#46,
    /// Indiana Jones and the Great Circle, which worked until 0.11.8).
    pub fn reshade_engine_problem(&self) -> Option<String> {
        (self.api == Api::Vulkan).then(|| {
            "This is a Vulkan game, so the ReShade engine cannot reach it: ReShade hooks \
             Vulkan through a registered layer, not through the dxgi.dll installed beside \
             the exe, and nothing here would ever load. Use the OptiScaler engine, which \
             does cover Vulkan. To use ReShade anyway, run its own setup, point it at this \
             exe and choose Vulkan, then follow DLSS5-Feeder's Vulkan instructions."
                .to_owned()
        })
    }

    pub fn is32(&self) -> bool {
        self.bitness == 32
    }
    /// Where the DLSS 5 add-on and the NVIDIA DLLs live: beside the exe for a
    /// 64-bit game, in `host64\` for a 32-bit one.
    pub fn consumer_dir(&self) -> PathBuf {
        if self.is32() {
            self.game_dir().join(HOST_DIR)
        } else {
            self.game_dir().to_path_buf()
        }
    }
    pub fn needs_bridge(&self) -> bool {
        self.mode == Mode::Native && (self.api == Api::Dx11 || bridge_override())
    }
    pub fn complete(&self) -> bool {
        match self.mode {
            Mode::Feeder => {
                self.reshade
                    && self.headers
                    && self.feeder
                    && self.lumenite
                    && self.dlss5_addon
                    && self.dlssnr
                    && self.dlss
                    && (!self.is32() || (self.host_exe && self.host_reshade))
            }
            Mode::Native => {
                // Either neural consumer counts: the RenoDX add-on, or the
                // experimental Neural Upstream one that stands in its place.
                (self.opti && self.dlssnr)
                    || (self.reshade
                        && (self.dlss5_addon || self.upstream)
                        && self.dlssnr
                        && (!self.needs_bridge() || self.bridge))
            }
        }
    }
}

pub fn inspect(exe: &Path) -> Result<GameStatus> {
    if !exe.is_file() {
        bail!("game executable not found: {}", exe.display());
    }
    let d = exe.parent().context("exe has no parent directory")?;
    let bitness = exe_bitness(exe)?;
    let shaders = join_ci(d, &["reshade-shaders", "Shaders"]);
    let textures = join_ci(d, &["reshade-shaders", "Textures"]);
    let mut problems = Vec::new();
    let anticheat = detect_anticheat(d).or_else(|| known_anticheat_exe(exe));
    if let Some(ac) = anticheat {
        if !ignore_anticheat() {
            problems.push(format!(
                "{ac} anti-cheat found in this game. ReShade add-on injection is what it detects: kick at best, ban at worst. Refused."
            ));
        }
    }
    let gpu = gpu::best();
    let skip_gpu = std::env::var_os("DLSS5ONECLICK_SKIP_GPU_CHECK").is_some();
    if let Some((g, t)) = &gpu {
        if !t.can_run() && !skip_gpu {
            problems.push(format!(
                "GPU is {} ({}): the DLSS 5 model runs on NVIDIA RTX only (it needs tensor cores and NGX). Misdetected? Set DLSS5ONECLICK_SKIP_GPU_CHECK=1.",
                g.name,
                t.label()
            ));
        }
    }
    // A d3d9.dll is usually a wrapper this tool cannot work behind — except
    // dgVoodoo2, which is exactly how a D3D9 game reaches D3D11 and then the
    // Feeder (verified working on Dead or Alive 5 Last Round, #17).
    if file_ci(d, "d3d9.dll") && !file_ci(d, RESHADE_PROXY) && !is_dgvoodoo(d) {
        problems.push(
            "A d3d9.dll proxy is present that is not dgVoodoo2. DirectX 9 itself is not a dead              end -- DLSS 5 needs a D3D11/12 device, and dgVoodoo2 provides one, which is how a              D3D9 game can work here (#17, #37) -- but this tool cannot install behind another              wrapper. Replace it with dgVoodoo 2.87.3 (MS\\x86\\D3D9.dll plus dgVoodoo.conf,              OutputAPI = bestavailable) and run Install again."
                .into(),
        );
    }
    let mut api = detect_api(exe);
    // dgVoodoo2 is the route this tool tells D3D9 users to take: it presents the
    // game as D3D11, which is what ReShade and the Feeder then attach to. Once it
    // is in place the game is a D3D11 game at run time, so refusing it here left
    // people who had followed the instructions with nowhere to go (Spore, #56).
    if api == Api::Dx9 && is_dgvoodoo(d) {
        api = Api::Dx11;
    }
    if api == Api::Dx9 {
        problems.push(
            "This is a DirectX 9 game. DLSS needs a Direct3D 11 or 12 device, which D3D9 \
             never creates, so nothing here can attach to it as it stands -- and the game \
             loads the system d3d9.dll by name, so the dxgi.dll this tool installs is never \
             even asked for (no ReShade overlay, no ReShade.log). The route that works is \
             dgVoodoo 2.87.3 first: it turns D3D9 into D3D11, and everything else follows \
             from there. Put its D3D9.dll from the MS/x86 folder beside the exe with \
             OutputAPI = bestavailable, confirm the game still starts, then run Install again."
                .into(),
        );
    }
    let is32 = bitness == 32;
    if is32 && api == Api::Dx12 {
        problems.push(
            "32-bit game on Direct3D 12: DLSS5-Feeder's 32-bit add-on covers Direct3D 9 (through dgVoodoo2), 10 and 11, but not 12."
                .into(),
        );
    }
    // 32-bit: NGX is 64-bit only, so the game can never carry its own DLSS;
    // everything 64-bit goes to host64\ and the Feeder's addon32 sits in-game.
    let cdir = if is32 {
        join_ci(d, &[HOST_DIR])
    } else {
        d.to_path_buf()
    };
    let feeder = if is32 {
        file_ci(d, FEEDER_ADDON32) && file_ci(&shaders, FEEDER_FX)
    } else {
        file_ci(d, FEEDER_ADDON) && file_ci(&shaders, FEEDER_FX)
    };
    let mode_detected = if !is32 && game_ships_dlss(d) {
        Mode::Native
    } else {
        Mode::Feeder
    };
    let mode = if is32 {
        Mode::Feeder
    } else {
        mode_override().unwrap_or(mode_detected)
    };
    let renodx_mod = fs::read_to_string(d.join(RENODX_MANIFEST))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|n| !n.is_empty() && d.join(n).is_file());
    Ok(GameStatus {
        mode,
        api,
        bridge: file_ci(d, BRIDGE_ADDON) || file_ci(d, "dlss5-dx11-bridge.addon64"),
        opti: file_ci(d, OPTI_MANIFEST),
        upstream: file_ci(d, UPSTREAM_ADDON),
        gpu,
        exe: exe.to_path_buf(),
        bitness,
        reshade: !file_ci(d, OPTI_MANIFEST) && is_reshade_dll(&join_ci(d, &[RESHADE_PROXY])),
        headers: RESHADE_HEADERS.iter().all(|h| file_ci(&shaders, h)),
        feeder,
        lumenite: file_ci(&shaders, LUMENITE_KERNEL_FX) && file_ci(&textures, LUMENITE_BLUENOISE),
        dlss5_addon: file_ci(&cdir, DLSS5_ADDON),
        dlssnr: file_ci(&cdir, DLSSNR_DLL),
        dlss: file_ci(&cdir, DLSS_DLL),
        mode_detected,
        host_exe: is32 && file_ci(&cdir, HOST_EXE),
        host_reshade: is32 && is_reshade_dll(&join_ci(&cdir, &[RESHADE_PROXY])),
        re_engine: file_ci(d, RE_ENGINE_PAK),
        reframework: file_ci(d, REFRAMEWORK_DLL),
        renodx_mod: renodx_mod.clone(),
        foreign_renodx: crate::renodx::foreign_mods(d, renodx_mod.as_deref()),
        mfg: file_ci(d, crate::mfg::MFG_MANIFEST),
        has_fg: crate::mfg::has_streamline_fg(d),
        anticheat,
        problems,
    })
}

/// Helper/launcher executables that are never the game.
const NOT_GAME: [&str; 15] = [
    "unitycrashhandler",
    "unrealcefsubprocess",
    "crashreportclient",
    "easyanticheat",
    "vcredist",
    "vc_redist",
    "dxwebsetup",
    "dxsetup",
    "oalinst",
    "ue4prereqsetup",
    "ueprereqsetup",
    "installer",
    "uninstall",
    "unins",
    "setup",
];

fn is_helper_name(stem_lower: &str) -> bool {
    NOT_GAME.iter().any(|n| stem_lower.contains(n))
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Candidate game executables in `dir`, best first.
///
/// Looks in the folder itself and in any `*/Binaries/Win64/` (Unreal layout,
/// where ReShade must sit next to the `-Shipping.exe`, not the root launcher).
/// Prefers 64-bit PEs, drops known helpers, then ranks: Unreal shipping
/// exe > name matches the folder name > larger file. A folder with no 64-bit
/// game at all falls back to its 32-bit exes, which the feeder drives through
/// its host64 helper (Max Payne, #17).
pub fn find_game_exes(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut push_dir = |d: &Path| {
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                // Not every game launcher is named .exe: Aion ships aion.bin,
                // which is an ordinary PE (#16). Anything that is not really a
                // PE is dropped by the bitness read further down, so widening
                // the extension costs nothing.
                if p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("exe") || x.eq_ignore_ascii_case("bin"))
                    && p.is_file()
                {
                    found.push(p);
                }
            }
        }
    };
    push_dir(dir);
    // Subfolders that never hold a launch exe, skipped at every level.
    let skip = |p: &Path| {
        let n = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(
            n.as_str(),
            "engine"
                | "content"
                | "saved"
                | "intermediate"
                | "reshade-shaders"
                | "_commonredist"
                | "commonredist"
                | "redist"
                | "redistributables"
        ) || n.ends_with("_data")
    };
    // Down to four levels, which is where games actually put the launch exe:
    // bin/x64, Game/Binaries/Win64, and ph_ft/work/bin/x64 (Dying Light: The
    // Beast, #43). Engine and content trees are skipped at every level.
    fn walk(d: &Path, depth: u8, skip: &dyn Fn(&Path) -> bool, out: &mut dyn FnMut(&Path)) {
        if depth == 0 {
            return;
        }
        let Ok(rd) = fs::read_dir(d) else {
            return;
        };
        for sub in rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !skip(p))
        {
            out(&sub);
            walk(&sub, depth - 1, skip, out);
        }
    }
    walk(dir, 4, &skip, &mut push_dir);
    // The Engine tree is skipped above because it is full of helper exes, but
    // Satisfactory keeps its shipping exe in exactly one place inside it (#29).
    let eng = join_ci(dir, &["Engine", "Binaries", "Win64"]);
    if eng.is_dir() {
        push_dir(&eng);
    }
    let folder = norm(dir.file_name().and_then(|n| n.to_str()).unwrap_or(""));
    // Import-table scoring reads each exe in full; only pay for it when there
    // is an actual choice to make.
    let contested = found.len() > 1;
    let mut scored: Vec<(u8, i64, PathBuf)> = found
        .into_iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let bits = exe_bitness(&p).ok()?;
            if is_helper_name(&stem) {
                return None;
            }
            let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0) as i64;
            let mut score: i64 = 0;
            let n = norm(&stem);
            // An Unreal `-Shipping.exe` is always the real game; the root exe
            // named after the folder (Expedition33_Steam.exe, 700 KB) is a bootstrapper.
            if stem.ends_with("-shipping") {
                score += 2_000_000_000;
            }
            if !folder.is_empty()
                && (n == folder || n.starts_with(&folder) || folder.starts_with(&n))
            {
                score += 1_000_000_000;
            }
            // Between sibling variants, an exe whose own import table proves a
            // D3D renderer beats one that does not: BG3 ships bg3.exe (Vulkan,
            // no D3D imports) beside bg3_dx11.exe, and only the D3D11 one can
            // carry ReShade and the DX11 bridge. Outranks size, yields to the
            // -Shipping and folder-name signals (Witcher 3's two D3D exes both
            // get it, so their order is unchanged).
            if contested && matches!(classify_imports(&pe_imports(&p)), Api::Dx10 | Api::Dx11 | Api::Dx12) {
                score += 500_000_000;
            }
            score += size.min(400_000_000);
            Some((bits, score, p))
        })
        .collect();
    // A 32-bit exe sitting beside a 64-bit one is a tool, not the game, so
    // 64-bit wins whenever one exists. Only a folder with nothing 64-bit in it
    // (Max Payne, #17) falls back to 32-bit.
    if scored.iter().any(|s| s.0 == 64) {
        scored.retain(|s| s.0 == 64);
    }
    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.into_iter().map(|(_, _, p)| p).collect()
}

/// Accepts either a game exe or a game folder; returns the exe to use plus
/// every candidate found (empty when the input was already an exe).
/// Did this tool install into this folder? True when any marker or manifest
/// it writes is present. Used to group those games together (#nn) and to
/// decide whether a component may be refreshed.
pub fn installed_by_tool(dir: &Path) -> bool {
    [
        OPTI_MANIFEST,
        RENODX_MANIFEST,
        REFRAMEWORK_MARKER,
        DLSS_MARKER,
        DLSSNR_MARKER,
        RESHADE_MARKER,
        FEEDER_MARKER,
    ]
    .iter()
    .any(|m| dir.join(m).is_file())
}

pub fn resolve_target(input: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    if input.is_file() {
        return Ok((input.to_path_buf(), Vec::new()));
    }
    if input.is_dir() {
        let c = find_game_exes(input);
        return match c.first() {
            Some(first) => Ok((first.clone(), c)),
            None => bail!("no game executable found in {}", input.display()),
        };
    }
    bail!("not found: {}", input.display())
}

#[cfg(test)]
pub mod testutil {
    use super::*;

    pub fn make_pe(path: &Path, machine: u16) -> PathBuf {
        let mut head = vec![0u8; 0x40];
        head[..2].copy_from_slice(b"MZ");
        head[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let mut pe = b"PE\0\0".to_vec();
        pe.extend_from_slice(&machine.to_le_bytes());
        pe.extend_from_slice(&[0u8; 18]);
        head.extend_from_slice(&pe);
        fs::write(path, head).unwrap();
        path.to_path_buf()
    }

    /// A minimal PE32+ whose import table names `dlls`, padded to `pad` bytes
    /// so size-based scoring can be exercised. Parsed by `pe_imports`.
    pub fn make_pe_with_imports(path: &Path, dlls: &[&str], pad: usize) -> PathBuf {
        let mut head = vec![0u8; 0x40];
        head[..2].copy_from_slice(b"MZ");
        head[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let coff_start = 0x40;
        let opt_start = coff_start + 24; // sig(4) + coff(20)
        let opt_size = 240u16; // PE32+ standard: 112 + 16 dirs * 8
        let sec_start = opt_start + opt_size as usize;
        let raw_start = sec_start + 40; // one section header
        let va = 0x1000u32;

        // Section payload: import descriptors (20 B each + null), then names.
        let ndesc = dlls.len();
        let names_off = (ndesc + 1) * 20;
        let mut payload = vec![0u8; names_off];
        let mut name_pos = names_off;
        for (i, dll) in dlls.iter().enumerate() {
            let name_rva = va + name_pos as u32;
            payload[i * 20 + 12..i * 20 + 16].copy_from_slice(&name_rva.to_le_bytes());
            payload.extend_from_slice(dll.as_bytes());
            payload.push(0);
            name_pos = payload.len();
        }

        let mut out = head;
        out.extend_from_slice(b"PE\0\0");
        let mut coff = Vec::new();
        coff.extend_from_slice(&PE_X64.to_le_bytes()); // machine
        coff.extend_from_slice(&1u16.to_le_bytes()); // one section
        coff.extend_from_slice(&[0u8; 12]); // ts, symtab, nsyms
        coff.extend_from_slice(&opt_size.to_le_bytes());
        coff.extend_from_slice(&0u16.to_le_bytes()); // characteristics
        out.extend_from_slice(&coff);
        let mut opt = vec![0u8; opt_size as usize];
        opt[..2].copy_from_slice(&0x20Bu16.to_le_bytes()); // PE32+
        opt[120..124].copy_from_slice(&va.to_le_bytes()); // import dir RVA
        opt[124..128].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&opt);
        let mut sec = vec![0u8; 40];
        sec[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes()); // vsize
        sec[12..16].copy_from_slice(&va.to_le_bytes()); // vaddr
        sec[16..20].copy_from_slice(&(payload.len() as u32).to_le_bytes()); // raw size
        sec[20..24].copy_from_slice(&(raw_start as u32).to_le_bytes()); // raw ptr
        out.extend_from_slice(&sec);
        assert_eq!(out.len(), raw_start);
        out.extend_from_slice(&payload);
        if out.len() < pad {
            out.resize(pad, 0);
        }
        fs::write(path, out).unwrap();
        path.to_path_buf()
    }

    /// A minimal PE32+ with one import descriptor, so the import reader can be
    /// tested on a file whose contents are known exactly.
    pub fn make_pe_importing(path: &Path, dll: &str, fns: &[&str]) -> PathBuf {
        make_pe_importing_many(path, &[(dll, fns)])
    }

    /// The same, with one descriptor per (DLL, functions) pair.
    pub fn make_pe_importing_many(path: &Path, dlls: &[(&str, &[&str])]) -> PathBuf {
        // One section mapped 1:1 (RVA == file offset) keeps the maths trivial.
        const SEC: usize = 0x400;
        let mut img = vec![0u8; SEC];
        img[..2].copy_from_slice(b"MZ");
        img[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        let pe = 0x80usize;
        img[pe..pe + 4].copy_from_slice(b"PE\0\0");
        img[pe + 4..pe + 6].copy_from_slice(&PE_X64.to_le_bytes());
        img[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes()); // 1 section
        let opt_size: u16 = 240;
        img[pe + 20..pe + 22].copy_from_slice(&opt_size.to_le_bytes());
        let opt = pe + 24;
        img[opt..opt + 2].copy_from_slice(&0x20Bu16.to_le_bytes()); // PE32+

        let mut blob: Vec<u8> = Vec::new();
        let at = |b: &Vec<u8>| (SEC + b.len()) as u32;
        // Names and thunks first, one set per DLL; the descriptors follow.
        let mut parts: Vec<(u32, u32)> = Vec::new(); // (dll name RVA, thunk RVA)
        for (dll, fns) in dlls {
            let mut name_rvas = Vec::new();
            for f in *fns {
                name_rvas.push(at(&blob));
                blob.extend_from_slice(&0u16.to_le_bytes());
                blob.extend_from_slice(f.as_bytes());
                blob.push(0);
            }
            let dll_rva = at(&blob);
            blob.extend_from_slice(dll.as_bytes());
            blob.push(0);
            while !blob.len().is_multiple_of(8) {
                blob.push(0);
            }
            let thunk_rva = at(&blob);
            for r in &name_rvas {
                blob.extend_from_slice(&(*r as u64).to_le_bytes());
            }
            blob.extend_from_slice(&0u64.to_le_bytes()); // terminator
            parts.push((dll_rva, thunk_rva));
        }
        let desc_rva = at(&blob);
        for (dll_rva, thunk_rva) in &parts {
            blob.extend_from_slice(&thunk_rva.to_le_bytes()); // OriginalFirstThunk
            blob.extend_from_slice(&[0u8; 8]); // TimeDateStamp, ForwarderChain
            blob.extend_from_slice(&dll_rva.to_le_bytes()); // Name
            blob.extend_from_slice(&thunk_rva.to_le_bytes()); // FirstThunk
        }
        blob.extend_from_slice(&[0u8; 20]); // null descriptor

        // PE32+ data directories start at optional-header offset 112, and the
        // import table is index 1, so it sits eight bytes further in.
        img[opt + 120..opt + 124].copy_from_slice(&desc_rva.to_le_bytes());
        // Section header: VirtualAddress == PointerToRawData == SEC.
        let sh = opt + opt_size as usize;
        img[sh + 8..sh + 12].copy_from_slice(&(blob.len() as u32).to_le_bytes()); // VirtualSize
        img[sh + 12..sh + 16].copy_from_slice(&(SEC as u32).to_le_bytes()); // VirtualAddress
        img[sh + 16..sh + 20].copy_from_slice(&(blob.len() as u32).to_le_bytes()); // SizeOfRawData
        img[sh + 20..sh + 24].copy_from_slice(&(SEC as u32).to_le_bytes()); // PointerToRawData
        img.extend_from_slice(&blob);
        fs::write(path, &img).unwrap();
        path.to_path_buf()
    }

    pub fn make_reshade_dll(path: &Path) -> PathBuf {
        let mut b = b"MZ".to_vec();
        b.extend(std::iter::repeat_n(0u8, 1 << 20));
        b.extend_from_slice(b"ReShade");
        b.extend_from_slice(&[0u8; 16]);
        fs::write(path, b).unwrap();
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {

    /// Unreal keeps its Agility runtime in a D3D12 subfolder, Unity players put
    /// it directly beside the exe. Missing the second layout is what made a
    /// 0x887E0003 look unexplainable (dlss5-bridge#24).
    #[test]
    fn agility_redist_found_in_both_layouts() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert!(has_agility_redist(d).is_none());

        fs::write(d.join("D3D12Core.dll"), b"x").unwrap();
        assert_eq!(has_agility_redist(d), Some(d.join("D3D12Core.dll")));

        fs::create_dir(d.join("D3D12")).unwrap();
        fs::write(d.join("D3D12").join("D3D12Core.dll"), b"x").unwrap();
        // The subfolder wins when a game somehow carries both.
        assert_eq!(
            has_agility_redist(d),
            Some(d.join("D3D12").join("D3D12Core.dll"))
        );
    }
    use super::testutil::*;
    use super::*;

    #[test]
    fn existing_ci_and_join_ci_reuse_on_disk_casing() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        fs::create_dir_all(d.join("Reshade-Shaders").join("SHADERS")).unwrap();
        assert!(existing_ci(d, "RESHADE-shaders")
            .unwrap()
            .ends_with("Reshade-Shaders"));
        assert!(existing_ci(d, "nope").is_none());
        let j = join_ci(d, &["reshade-shaders", "Shaders", "New.fx"]);
        assert_eq!(j, d.join("Reshade-Shaders").join("SHADERS").join("New.fx"));
    }

    #[test]
    fn inspect_finds_pieces_in_differently_cased_tree() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        make_pe(&d.join("game.exe"), PE_X64);
        let shaders = d.join("RESHADE-SHADERS").join("shaders");
        fs::create_dir_all(&shaders).unwrap();
        for h in RESHADE_HEADERS {
            fs::write(shaders.join(h.to_uppercase()), b"x").unwrap();
        }
        fs::write(d.join("DLSS5-FEED.ADDON64"), b"x").unwrap();
        fs::write(shaders.join("dlss5_feed.FX"), b"x").unwrap();
        let st = inspect(&d.join("game.exe")).unwrap();
        assert!(st.headers, "headers must be found despite casing");
        assert!(st.feeder, "feeder must be found despite casing");
    }

    #[test]
    fn bitness_x64_and_x86() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            exe_bitness(&make_pe(&t.path().join("a.exe"), PE_X64)).unwrap(),
            64
        );
        assert_eq!(
            exe_bitness(&make_pe(&t.path().join("b.exe"), PE_X86)).unwrap(),
            32
        );
    }

    #[test]
    fn bitness_rejects_non_pe() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("x.exe");
        fs::write(&p, b"hello").unwrap();
        assert!(exe_bitness(&p).is_err());
    }

    #[test]
    fn reshade_dll_needs_marker_and_size() {
        let t = tempfile::tempdir().unwrap();
        let small = t.path().join("dxgi.dll");
        fs::write(&small, b"ReShade").unwrap();
        assert!(!is_reshade_dll(&small));
        assert!(is_reshade_dll(&make_reshade_dll(
            &t.path().join("real.dll")
        )));
    }

    #[test]
    fn inspect_empty_and_32bit() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let st = inspect(&make_pe(&t.path().join("game.exe"), PE_X64)).unwrap();
        assert_eq!(st.bitness, 64);
        assert!(
            !st.reshade
                && !st.headers
                && !st.feeder
                && !st.lumenite
                && !st.dlss5_addon
                && !st.dlssnr
                && !st.dlss
        );
        assert!(!st.complete());
        assert!(st.problems.is_empty());

        let st = inspect(&make_pe(&t.path().join("g32.exe"), PE_X86)).unwrap();
        assert!(st.is32() && st.mode == Mode::Feeder);
        assert!(!st.problems.iter().any(|p| p.contains("32-bit")));
    }

    /// Max Payne is a 32-bit game and has no 64-bit exe at all; picking the
    /// folder used to find nothing, so it only worked when the exe was chosen
    /// by hand (#17). The feeder drives 32-bit games through its host64 helper.
    #[test]
    fn find_game_exes_falls_back_to_32bit_when_no_64bit_exists() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Max Payne");
        fs::create_dir_all(&d).unwrap();
        make_pe(&d.join("maxpayne.exe"), PE_X86);
        assert_eq!(find_game_exes(&d), vec![d.join("maxpayne.exe")]);
    }

    #[test]
    fn find_game_exes_skips_helpers_and_prefers_folder_name() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Fell & Sell");
        fs::create_dir_all(&d).unwrap();
        make_pe(&d.join("UnityCrashHandler64.exe"), PE_X64);
        make_pe(&d.join("tool32.exe"), PE_X86);
        make_pe(&d.join("Fell & Sell.exe"), PE_X64);
        let c = find_game_exes(&d);
        assert_eq!(c, vec![d.join("Fell & Sell.exe")]);
        let (exe, all) = resolve_target(&d).unwrap();
        assert_eq!(exe, d.join("Fell & Sell.exe"));
        assert_eq!(all.len(), 1);
    }

    /// Satisfactory (Epic): the launcher names FactoryGameEGS.exe in the root,
    /// but the real game is the shipping exe under Engine\Binaries\Win64 (#29).
    /// A Direct3D 10 game imports d3d10_1.dll, and often d3d9.dll too for its
    /// D3DPERF debug markers - which is how DMC4:SE ends up looking like a
    /// DirectX 9 game to a naive scan.
    /// OptiScaler's dxgi.dll says "ReShade" because it can load ReShade; only
    /// a real ReShade build also carries crosire's name (#the update refusal).
    #[test]
    fn optiscaler_is_not_mistaken_for_reshade() {
        assert!(is_reshade_image(
            b"...crosire's ReShade version '6.8.0.2155'..."
        ));
        assert!(!is_reshade_image(
            b"OptiScaler ... LoadReshade ... ReShade64.dll ..."
        ));
        // A ReShade build without the author's name is still ReShade.
        assert!(is_reshade_image(b"ReShade effect runtime"));
        assert!(!is_reshade_image(b"some other dxgi wrapper"));
    }

    /// A Vulkan-only game imports vulkan-1.dll and no Direct3D; the dxgi.dll
    /// proxy can never load in one, so it has to be named rather than
    /// reported as "API unknown, assuming DX12" (#6).
    /// Red Dead Redemption 2 imports d3d9.dll only for the D3DPERF debug
    /// markers and renders with something far newer, so v0.11.17's renderer
    /// scan read it as a DirectX 9 game and refused to install (#53).
    #[test]
    fn d3d9_debug_markers_alone_are_not_a_d3d9_game() {
        let t = tempfile::tempdir().unwrap();
        let markers = testutil::make_pe_importing(
            &t.path().join("markers.exe"),
            "d3d9.dll",
            &["D3DPERF_BeginEvent", "D3DPERF_EndEvent"],
        );
        assert_eq!(
            pe_import_fns(&markers, "d3d9.dll"),
            vec!["d3dperf_beginevent", "d3dperf_endevent"]
        );
        assert_eq!(pe_imports(&markers), vec!["d3d9.dll"]);
        assert_eq!(detect_api(&markers), Api::Unknown);

        // A real Direct3D 9 renderer still reads as one.
        let real = testutil::make_pe_importing(
            &t.path().join("real.exe"),
            "d3d9.dll",
            &["Direct3DCreate9", "D3DPERF_BeginEvent"],
        );
        assert_eq!(detect_api(&real), Api::Dx9);

        // Direct3D 9 predates DXGI, so a PE importing both is not a D3D9 game
        // however it uses d3d9.dll - it creates its real device at runtime.
        let dxgi = testutil::make_pe_importing_many(
            &t.path().join("dxgi.exe"),
            &[
                ("d3d9.dll", &["Direct3DCreate9"][..]),
                ("dxgi.dll", &["CreateDXGIFactory1"][..]),
            ],
        );
        assert_eq!(detect_api(&dxgi), Api::Unknown);

        // Red Dead Redemption 2's own import table (#53): a real
        // Direct3DCreate9Ex import, no dxgi.dll at all, and the FSR2 and Reflex
        // libraries for D3D12 and Vulkan beside it.
        let rdr2 = testutil::make_pe_importing_many(
            &t.path().join("RDR2.exe"),
            &[
                ("d3d9.dll", &["Direct3DCreate9Ex"][..]),
                ("nvlowlatencyvk.dll", &["NvLL_VK_Initialize"][..]),
                (
                    "ffx_fsr2_api_dx12_x64.dll",
                    &["ffxFsr2GetInterfaceDX12"][..],
                ),
                ("ffx_fsr2_api_vk_x64.dll", &["ffxFsr2GetInterfaceVK"][..]),
            ],
        );
        assert_eq!(detect_api(&rdr2), Api::Unknown);
    }

    #[test]
    fn classify_imports_reads_d3d9() {
        // Aion imports d3d9.dll (through XRenderD3D9.dll) and nothing newer;
        // before this it read as "unknown, assume DX12" and the tool installed
        // a dxgi.dll the game never loads (#16).
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(classify_imports(&s(&["d3d9.dll"])), Api::Dx9);
        // A game that also uses something newer is not a D3D9 game.
        assert_eq!(classify_imports(&s(&["d3d9.dll", "d3d11.dll"])), Api::Dx11);
        assert_eq!(classify_imports(&s(&["d3d9.dll", "d3d12.dll"])), Api::Dx12);
    }

    #[test]
    fn classify_imports_reads_vulkan() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(classify_imports(&s(&["vulkan-1.dll"])), Api::Vulkan);
        // A game offering both still takes the Direct3D path.
        assert_eq!(
            classify_imports(&s(&["vulkan-1.dll", "d3d12.dll"])),
            Api::Dx12
        );
        assert_eq!(
            classify_imports(&s(&["vulkan-1.dll", "d3d11.dll"])),
            Api::Dx11
        );
    }

    #[test]
    fn classify_imports_reads_d3d10() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            classify_imports(&s(&["d3d10_1.dll", "d3d9.dll"])),
            Api::Dx10
        );
        assert_eq!(classify_imports(&s(&["d3d10.dll"])), Api::Dx10);
        // The newer APIs still win when both are imported.
        assert_eq!(
            classify_imports(&s(&["d3d10_1.dll", "d3d11.dll"])),
            Api::Dx11
        );
        assert_eq!(
            classify_imports(&s(&["d3d10_1.dll", "d3d12.dll"])),
            Api::Dx12
        );
        assert_eq!(classify_imports(&s(&["kernel32.dll"])), Api::Unknown);
    }

    #[test]
    fn find_game_exes_finds_shipping_exe_under_engine_binaries() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("SatisfactoryEarlyAccess");
        let eng = d.join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&eng).unwrap();
        make_pe(&d.join("FactoryGameEGS.exe"), PE_X64);
        let real = make_pe(&eng.join("FactoryGameEGS-Win64-Shipping.exe"), PE_X64);
        make_pe(&eng.join("CrashReportClient.exe"), PE_X64);
        let (exe, all) = resolve_target(&d).unwrap();
        assert_eq!(exe, real);
        assert!(all.contains(&d.join("FactoryGameEGS.exe")));
    }

    /// BG3 ships bg3.exe (Vulkan, no D3D imports) beside bg3_dx11.exe: only
    /// the D3D11 exe can carry ReShade and the DX11 bridge, so it must win
    /// even against a larger Vulkan sibling.
    #[test]
    fn find_game_exes_prefers_d3d_importing_sibling() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Baldurs Gate 3").join("bin");
        fs::create_dir_all(&d).unwrap();
        make_pe_with_imports(&d.join("bg3.exe"), &["vulkan-1.dll"], 3_000_000);
        let dx11 = make_pe_with_imports(&d.join("bg3_dx11.exe"), &["d3d11.dll"], 2_000_000);
        let (exe, all) = resolve_target(t.path().join("Baldurs Gate 3").as_path()).unwrap();
        assert_eq!(exe, dx11);
        assert_eq!(all.len(), 2);
        // Sanity: the helper's import table really parses.
        assert_eq!(detect_api(&dx11), Api::Dx11);
    }

    #[test]
    fn bridge_override_forces_needs_bridge() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), PE_X64);
        fs::write(t.path().join(DLSS_DLL), b"x").unwrap(); // native mode
        let st = inspect(&exe).unwrap();
        assert_eq!(st.mode, Mode::Native);
        assert_ne!(st.api, Api::Dx11);
        assert!(!st.needs_bridge());
        std::env::set_var(BRIDGE_ENV, "1");
        let forced = inspect(&exe).unwrap().needs_bridge();
        std::env::remove_var(BRIDGE_ENV);
        assert!(forced);
    }

    #[test]
    fn find_game_exes_skips_redistributables_and_setup_exes() {
        // Red Dead Redemption 2: RDR2.exe in the root, a much larger
        // Social-Club-Setup.exe under Redistributables\ (#22).
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Red Dead Redemption 2");
        fs::create_dir_all(d.join("Redistributables").join("SocialClub")).unwrap();
        let game = make_pe(&d.join("RDR2.exe"), PE_X64);
        let big = make_pe(
            &d.join("Redistributables")
                .join("SocialClub")
                .join("Social-Club-Setup.exe"),
            PE_X64,
        );
        fs::write(&big, [b"MZ".as_slice(), &[0u8; 4_000_000]].concat()).unwrap();
        make_pe(
            &d.join("Redistributables")
                .join("SocialClub")
                .join("Social-Club-Setup.exe"),
            PE_X64,
        );
        let found = find_game_exes(&d);
        assert_eq!(found.first(), Some(&game));
        assert!(!found
            .iter()
            .any(|p| p.to_string_lossy().contains("Redistributables")));
    }

    #[test]
    fn find_game_exes_accepts_bin_launchers() {
        // Aion's launcher is aion.bin, an ordinary PE with an unusual
        // extension; before this it was invisible to the scan (#16).
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Aion");
        fs::create_dir_all(&d).unwrap();
        let exe = d.join("aion.bin");
        make_pe(&exe, PE_X64);
        assert_eq!(find_game_exes(&d), vec![exe]);

        // A .bin that is not a PE at all is still ignored.
        let t2 = tempfile::tempdir().unwrap();
        let d2 = t2.path().join("Game");
        fs::create_dir_all(&d2).unwrap();
        fs::write(d2.join("data.bin"), b"not a PE, just data").unwrap();
        assert!(find_game_exes(&d2).is_empty());
    }

    #[test]
    fn find_game_exes_finds_exe_four_levels_down() {
        // Dying Light: The Beast keeps its exe at ph_ft\work\bin\x64 (#43);
        // two levels of search reported "no 64-bit game executable found".
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Dying Light The Beast");
        let deep = d.join("ph_ft").join("work").join("bin").join("x64");
        fs::create_dir_all(&deep).unwrap();
        let exe = deep.join("DyingLightGame_TheBeast_x64_rwdi.exe");
        make_pe(&exe, PE_X64);
        assert_eq!(find_game_exes(&d), vec![exe]);

        // Five levels down is still out of reach, and content trees stay skipped.
        let t2 = tempfile::tempdir().unwrap();
        let d2 = t2.path().join("Game");
        let too_deep = d2.join("a").join("b").join("c").join("d").join("e");
        fs::create_dir_all(&too_deep).unwrap();
        make_pe(&too_deep.join("Game.exe"), PE_X64);
        let content = d2.join("Content").join("bin");
        fs::create_dir_all(&content).unwrap();
        make_pe(&content.join("Game.exe"), PE_X64);
        assert!(find_game_exes(&d2).is_empty());
    }

    #[test]
    fn find_game_exes_unreal_layout_prefers_shipping() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("SomeGame");
        let bin = d.join("SomeGame").join("Binaries").join("Win64");
        fs::create_dir_all(&bin).unwrap();
        make_pe(&d.join("SomeGame.exe"), PE_X64);
        make_pe(&bin.join("SomeGame-Win64-Shipping.exe"), PE_X64);
        make_pe(&bin.join("CrashReportClient.exe"), PE_X64);
        let c = find_game_exes(&d);
        assert_eq!(c[0], bin.join("SomeGame-Win64-Shipping.exe"));
        assert_eq!(c.len(), 2);
        assert!(resolve_target(&t.path().join("nope")).is_err());
        let empty = t.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(resolve_target(&empty).is_err());
    }

    #[test]
    fn native_mode_when_game_ships_dlss() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X64);
        let st = inspect(&exe).unwrap();
        assert_eq!(st.mode, Mode::Feeder);
        assert_eq!(st.api, Api::Unknown);
        // nested nvngx_dlss.dll (Unreal-style plugin folder) -> native
        let deep = d.join("Engine").join("Plugins").join("DLSS");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join(DLSS_DLL), b"x").unwrap();
        let st = inspect(&exe).unwrap();
        assert_eq!(st.mode, Mode::Native);
        assert!(!st.complete());
        // our own copy (with sidecar marker) does not count
        fs::remove_file(deep.join(DLSS_DLL)).unwrap();
        fs::write(d.join(DLSS_DLL), b"x").unwrap();
        fs::write(d.join(DLSS_MARKER), b"").unwrap();
        assert_eq!(inspect(&exe).unwrap().mode, Mode::Feeder);
        // Streamline runtime alone is a DLSS signal
        fs::write(d.join("sl.interposer.dll"), b"x").unwrap();
        assert_eq!(inspect(&exe).unwrap().mode, Mode::Native);
    }

    #[test]
    fn pe_imports_handles_stub_pe() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), PE_X64);
        assert!(pe_imports(&exe).is_empty());
        assert_eq!(detect_api(&exe), Api::Unknown);
    }

    /// A D3D9 game with dgVoodoo2 beside it is a D3D11 game at run time, and
    /// that is the route the refusal text itself sends people to. Refusing it
    /// anyway left Spore users stuck after doing exactly as told (#56).
    #[test]
    fn dgvoodoo_turns_a_d3d9_game_into_the_d3d11_path() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe =
            testutil::make_pe_importing(&d.join("SporeApp.exe"), "d3d9.dll", &["Direct3DCreate9"]);
        assert_eq!(detect_api(&exe), Api::Dx9);
        let st = inspect(&exe).unwrap();
        assert_eq!(st.api, Api::Dx9);
        assert!(st.problems.iter().any(|p| p.contains("DirectX 9 game")));

        // dgVoodoo2 in place: no refusal, and the D3D11 path from there on.
        fs::write(d.join("dgVoodoo.conf"), b"[General]").unwrap();
        fs::write(d.join("d3d9.dll"), b"MZ...dgVoodoo2 wrapper...").unwrap();
        let st = inspect(&exe).unwrap();
        assert_eq!(st.api, Api::Dx11);
        assert!(!st.problems.iter().any(|p| p.contains("DirectX 9 game")));
        assert!(!st.problems.iter().any(|p| p.contains("d3d9.dll proxy")));
    }

    #[test]
    fn dgvoodoo_d3d9_is_allowed_other_d3d9_is_not() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X86);
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        fs::write(d.join("d3d9.dll"), b"MZ some other wrapper").unwrap();
        assert!(inspect(&exe)
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("d3d9.dll proxy")));
        fs::write(d.join("dgVoodoo.conf"), b"[General]").unwrap();
        assert!(is_dgvoodoo(d));
        assert!(!inspect(&exe)
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("d3d9.dll proxy")));
        fs::remove_file(d.join("dgVoodoo.conf")).unwrap();
        fs::write(d.join("d3d9.dll"), b"MZ...dgVoodoo2 wrapper...").unwrap();
        assert!(is_dgvoodoo(d));
    }

    #[test]
    fn anticheat_detected_and_refused() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X64);
        assert_eq!(detect_anticheat(d), None);
        fs::create_dir_all(d.join("tools")).unwrap();
        fs::write(d.join("tools").join("GGSetup.exe"), b"x").unwrap();
        assert_eq!(detect_anticheat(d), Some("GameGuard"));
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let st = inspect(&exe).unwrap();
        assert!(st.problems.iter().any(|p| p.contains("GameGuard")));
        assert_eq!(st.anticheat, Some("GameGuard"));
        fs::remove_dir_all(d.join("tools")).unwrap();
        fs::create_dir_all(
            d.join("Game")
                .join("Binaries")
                .join("Win64")
                .join("EasyAntiCheat"),
        )
        .unwrap();
        assert_eq!(detect_anticheat(d), Some("Easy Anti-Cheat"));
        fs::remove_dir_all(d.join("Game")).unwrap();
        fs::write(d.join("Foo_BE.exe"), b"x").unwrap();
        assert_eq!(detect_anticheat(d), Some("BattlEye"));
    }

    /// Neural Upstream stands in for the RenoDX add-on, so an install that has
    /// it plus the model is complete without `renodx-dlss5.addon64` (#50).
    #[test]
    fn upstream_counts_as_the_neural_consumer() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), PE_X64);
        let mut st = inspect(&exe).unwrap();
        st.mode = Mode::Native;
        st.api = Api::Dx12;
        st.reshade = true;
        st.dlssnr = true;
        assert!(!st.complete());
        st.upstream = true;
        assert!(st.complete());
    }

    #[test]
    fn inspect_complete() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X64);
        make_reshade_dll(&d.join("dxgi.dll"));
        let sh = d.join("reshade-shaders").join("Shaders");
        let tx = d.join("reshade-shaders").join("Textures");
        fs::create_dir_all(&sh).unwrap();
        fs::create_dir_all(&tx).unwrap();
        for f in [FEEDER_ADDON, DLSS5_ADDON, DLSSNR_DLL, DLSS_DLL] {
            fs::write(d.join(f), b"x").unwrap();
        }
        for h in RESHADE_HEADERS {
            fs::write(sh.join(h), "// header").unwrap();
        }
        fs::write(sh.join(FEEDER_FX), "technique DLSS5_Feed {}").unwrap();
        fs::write(sh.join(LUMENITE_KERNEL_FX), "technique Lumenite_Kernel {}").unwrap();
        fs::write(tx.join(LUMENITE_BLUENOISE), b"png").unwrap();
        assert!(inspect(&exe).unwrap().complete());
    }
}
