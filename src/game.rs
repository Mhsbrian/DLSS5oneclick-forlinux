//! Game-folder inspection: exe bitness, ReShade presence, installed pieces.

use crate::gpu;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const PE_X64: u16 = 0x8664;
pub const PE_X86: u16 = 0x014C;

pub const FEEDER_ADDON: &str = "dlss5-feed.addon64";
pub const FEEDER_FX: &str = "DLSS5_Feed.fx";
pub const DLSS5_ADDON: &str = "renodx-dlss5.addon64";
pub const DLSSNR_DLL: &str = "nvngx_dlssnr.dll";
pub const DLSS_DLL: &str = "nvngx_dlss.dll";
pub const LUMENITE_KERNEL_FX: &str = "lumenite_Kernel.fx";
pub const LUMENITE_BLUENOISE: &str = "lumenite_bluenoise256.png";
pub const BRIDGE_ADDON: &str = "dlss5-bridge.addon64";
/// Files this tool wrote for an OptiScaler install, one path per line.
pub const OPTI_MANIFEST: &str = ".dlss5oneclick-optiscaler-manifest";
/// Sidecar written next to an `nvngx_dlss.dll` this tool placed, so it is never mistaken for the game's own.
pub const DLSS_MARKER: &str = "nvngx_dlss.dll.dlss5oneclick";
pub const RESHADE_PROXY: &str = "dxgi.dll";
/// Shader headers the official installer fetches from crosire/reshade-shaders (branch `slim`).
/// Not inside the setup exe. DLSS5_Feed.fx and every lumenite_*.fx include ReShade.fxh;
/// DLSS5_Feed.fx also includes DrawText.fxh; ReShadeUI.fxh is the standard companion.
pub const RESHADE_HEADERS: [&str; 3] = ["ReShade.fxh", "ReShadeUI.fxh", "DrawText.fxh"];
pub const RESHADE_INI: &str = "ReShade.ini";
pub const RESHADE_PRESET: &str = "ReShadePreset.ini";

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
        Ok(bytes) => bytes.windows(7).any(|w| w == b"ReShade"),
        Err(_) => false,
    }
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
    Dx11,
    Dx12,
    /// Neither d3d11.dll nor d3d12.dll is a static import (loaded at runtime, or DX9/Vulkan).
    Unknown,
}

impl Api {
    pub fn label(self) -> &'static str {
        match self {
            Api::Dx11 => "DX11",
            Api::Dx12 => "DX12",
            Api::Unknown => "API unknown, assuming DX12",
        }
    }
}

/// Lower-cased DLL names from the exe's static import table. Empty on any parse problem.
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

pub fn detect_api(exe: &Path) -> Api {
    fn classify(imports: &[String]) -> Api {
        let has = |n: &str| imports.iter().any(|i| i == n);
        if has("d3d12.dll") {
            Api::Dx12
        } else if has("d3d11.dll") {
            Api::Dx11
        } else {
            Api::Unknown
        }
    }
    let api = classify(&pe_imports(exe));
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
    for (_, dll) in dlls.into_iter().take(12) {
        match classify(&pe_imports(&dll)) {
            Api::Dx12 => return Api::Dx12,
            Api::Dx11 => seen_dx11 = true,
            Api::Unknown => {}
        }
    }
    if seen_dx11 {
        Api::Dx11
    } else {
        Api::Unknown
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
    walk(game_dir, 4)
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
    pub problems: Vec<String>,
}

impl GameStatus {
    pub fn game_dir(&self) -> &Path {
        self.exe.parent().expect("exe has a parent")
    }
    pub fn needs_bridge(&self) -> bool {
        self.mode == Mode::Native && self.api == Api::Dx11
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
            }
            Mode::Native => {
                (self.opti && self.dlssnr)
                    || (self.reshade
                        && self.dlss5_addon
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
    let shaders = d.join("reshade-shaders").join("Shaders");
    let textures = d.join("reshade-shaders").join("Textures");
    let mut problems = Vec::new();
    if let Some(ac) = detect_anticheat(d) {
        if std::env::var_os("DLSS5ONECLICK_IGNORE_ANTICHEAT").is_none() {
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
                "GPU is {} ({}): the DLSS 5 model runs on NVIDIA RTX only (it needs tensor cores and NGX).",
                g.name,
                t.label()
            ));
        }
    }
    if bitness != 64 {
        problems.push("32-bit game: DLSS5-Feeder needs the host64 setup, which this tool does not automate yet.".into());
    }
    if d.join("d3d9.dll").is_file() && !d.join(RESHADE_PROXY).is_file() {
        problems
            .push("A d3d9.dll proxy is present; DirectX 9 games are not supported here.".into());
    }
    let feeder = d.join(FEEDER_ADDON).is_file() && shaders.join(FEEDER_FX).is_file();
    let mode = if game_ships_dlss(d) {
        Mode::Native
    } else {
        Mode::Feeder
    };
    let api = detect_api(exe);
    Ok(GameStatus {
        mode,
        api,
        bridge: d.join(BRIDGE_ADDON).is_file() || d.join("dlss5-dx11-bridge.addon64").is_file(),
        opti: d.join(OPTI_MANIFEST).is_file(),
        gpu,
        exe: exe.to_path_buf(),
        bitness,
        reshade: !d.join(OPTI_MANIFEST).is_file() && is_reshade_dll(&d.join(RESHADE_PROXY)),
        headers: RESHADE_HEADERS.iter().all(|h| shaders.join(h).is_file()),
        feeder,
        lumenite: shaders.join(LUMENITE_KERNEL_FX).is_file()
            && textures.join(LUMENITE_BLUENOISE).is_file(),
        dlss5_addon: d.join(DLSS5_ADDON).is_file(),
        dlssnr: d.join(DLSSNR_DLL).is_file(),
        dlss: d.join(DLSS_DLL).is_file(),
        problems,
    })
}

/// Helper/launcher executables that are never the game.
const NOT_GAME: [&str; 14] = [
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
/// Keeps 64-bit PEs only, drops known helpers, then ranks: name matches the
/// folder name > Unreal shipping exe > larger file.
pub fn find_game_exes(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut push_dir = |d: &Path| {
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) && p.is_file() {
                    found.push(p);
                }
            }
        }
    };
    push_dir(dir);
    // One and two levels down (bin/x64, bin/x64_dx12, Game/Binaries/Win64 ...), skipping
    // engine/content trees that never hold the launch exe.
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
        ) || n.ends_with("_data")
    };
    if let Ok(rd1) = fs::read_dir(dir) {
        for d1 in rd1
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !skip(p))
        {
            push_dir(&d1);
            if let Ok(rd2) = fs::read_dir(&d1) {
                for d2 in rd2
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && !skip(p))
                {
                    push_dir(&d2);
                    let win64 = d2.join("Win64");
                    if win64.is_dir() {
                        push_dir(&win64);
                    }
                }
            }
        }
    }
    let folder = norm(dir.file_name().and_then(|n| n.to_str()).unwrap_or(""));
    let mut scored: Vec<(i64, PathBuf)> = found
        .into_iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            if is_helper_name(&stem) || exe_bitness(&p).ok()? != 64 {
                return None;
            }
            let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0) as i64;
            let mut score: i64 = 0;
            let n = norm(&stem);
            if !folder.is_empty()
                && (n == folder || n.starts_with(&folder) || folder.starts_with(&n))
            {
                score += 1_000_000_000;
            }
            if stem.ends_with("-shipping") {
                score += 500_000_000;
            }
            score += size.min(400_000_000);
            Some((score, p))
        })
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored.into_iter().map(|(_, p)| p).collect()
}

/// Accepts either a game exe or a game folder; returns the exe to use plus
/// every candidate found (empty when the input was already an exe).
pub fn resolve_target(input: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    if input.is_file() {
        return Ok((input.to_path_buf(), Vec::new()));
    }
    if input.is_dir() {
        let c = find_game_exes(input);
        return match c.first() {
            Some(first) => Ok((first.clone(), c)),
            None => bail!("no 64-bit game executable found in {}", input.display()),
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
    use super::testutil::*;
    use super::*;

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
        assert!(st.problems.iter().any(|p| p.contains("32-bit")));
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
