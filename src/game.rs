//! Game-folder inspection: exe bitness, ReShade presence, installed pieces.

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
    let Ok(meta) = fs::metadata(path) else { return false };
    if !meta.is_file() || meta.len() < (1 << 20) {
        return false;
    }
    match fs::read(path) {
        Ok(bytes) => bytes.windows(7).any(|w| w == b"ReShade"),
        Err(_) => false,
    }
}

#[derive(Debug, Clone)]
pub struct GameStatus {
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
    pub fn complete(&self) -> bool {
        self.reshade && self.headers && self.feeder && self.lumenite && self.dlss5_addon && self.dlssnr && self.dlss
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
    if bitness != 64 {
        problems.push("32-bit game: DLSS5-Feeder needs the host64 setup, which this tool does not automate yet.".into());
    }
    if d.join("d3d9.dll").is_file() && !d.join(RESHADE_PROXY).is_file() {
        problems.push("A d3d9.dll proxy is present; DirectX 9 games are not supported here.".into());
    }
    Ok(GameStatus {
        exe: exe.to_path_buf(),
        bitness,
        reshade: is_reshade_dll(&d.join(RESHADE_PROXY)),
        headers: RESHADE_HEADERS.iter().all(|h| shaders.join(h).is_file()),
        feeder: d.join(FEEDER_ADDON).is_file() && shaders.join(FEEDER_FX).is_file(),
        lumenite: shaders.join(LUMENITE_KERNEL_FX).is_file()
            && textures.join(LUMENITE_BLUENOISE).is_file(),
        dlss5_addon: d.join(DLSS5_ADDON).is_file(),
        dlssnr: d.join(DLSSNR_DLL).is_file(),
        dlss: d.join(DLSS_DLL).is_file(),
        problems,
    })
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
        assert_eq!(exe_bitness(&make_pe(&t.path().join("a.exe"), PE_X64)).unwrap(), 64);
        assert_eq!(exe_bitness(&make_pe(&t.path().join("b.exe"), PE_X86)).unwrap(), 32);
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
        assert!(is_reshade_dll(&make_reshade_dll(&t.path().join("real.dll"))));
    }

    #[test]
    fn inspect_empty_and_32bit() {
        let t = tempfile::tempdir().unwrap();
        let st = inspect(&make_pe(&t.path().join("game.exe"), PE_X64)).unwrap();
        assert_eq!(st.bitness, 64);
        assert!(!st.reshade && !st.headers && !st.feeder && !st.lumenite && !st.dlss5_addon && !st.dlssnr && !st.dlss);
        assert!(!st.complete());
        assert!(st.problems.is_empty());

        let st = inspect(&make_pe(&t.path().join("g32.exe"), PE_X86)).unwrap();
        assert!(st.problems.iter().any(|p| p.contains("32-bit")));
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
