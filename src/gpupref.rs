//! Windows' per-application GPU preference.
//!
//! On a hybrid machine (laptop iGPU + dGPU, or a desktop with an AMD/Intel
//! display adapter alongside the NVIDIA card) Windows decides which adapter a
//! process runs on. When it picks the iGPU, NGX does not exist there and
//! `NVSDK_NGX_D3D12_Init` answers `0xBAD00001` (FeatureNotSupported) — which is
//! exactly what a reporter hit until he set the Feeder's helper to "High
//! performance" in Settings ▸ System ▸ Display ▸ Graphics (#25).
//!
//! Settings writes that choice to `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`
//! as a REG_SZ named by the executable's full path, holding `GpuPreference=2;`
//! (0 = let Windows decide, 1 = power saving, 2 = high performance; the same
//! numbering as `DXGI_GPU_PREFERENCE`). Other tokens may share the value
//! (`AppStatus=`, `AutoHDREnable=`, `SpecificAdapter=`), so only the
//! `GpuPreference` token is touched.

use std::path::Path;

pub const KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
pub const HIGH_PERFORMANCE: &str = "GpuPreference=2;";

/// True when a non-NVIDIA display adapter sits next to an NVIDIA one, i.e. the
/// machine can start a process on a GPU that has no NGX.
pub fn hybrid() -> bool {
    let list = crate::gpu::list();
    let nvidia = list
        .iter()
        .any(|g| g.name.to_ascii_lowercase().contains("nvidia"));
    nvidia
        && list
            .iter()
            .any(|g| crate::gpu::classify(&g.name) == crate::gpu::Tier::NotNvidia)
}

/// Replace (or add) the `GpuPreference` token, keeping every other token.
pub fn with_high_performance(existing: &str) -> String {
    let mut out: Vec<String> = existing
        .split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.starts_with("GpuPreference="))
        .map(|t| format!("{t};"))
        .collect();
    out.push(HIGH_PERFORMANCE.to_owned());
    out.join("")
}

/// Already asking for the high-performance GPU?
pub fn is_high_performance(value: &str) -> bool {
    value
        .split(';')
        .map(str::trim)
        .any(|t| t == "GpuPreference=2")
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn get(exe: &Path) -> Option<String> {
        let sub = wide(KEY);
        let name = wide(&exe.to_string_lossy());
        let mut buf = [0u16; 512];
        let mut size: u32 = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                sub.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut _,
                &mut size,
            )
        };
        if rc != 0 {
            return None;
        }
        let n = (size as usize / 2).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    /// Ask Windows to run `exe` on the high-performance GPU. Returns true when
    /// the value was written, false when it already said so.
    pub fn set_high_performance(exe: &Path) -> std::io::Result<bool> {
        let current = get(exe).unwrap_or_default();
        if is_high_performance(&current) {
            return Ok(false);
        }
        let value = with_high_performance(&current);
        let sub = wide(KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                sub.as_ptr(),
                0,
                std::ptr::null_mut(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
        let name = wide(&exe.to_string_lossy());
        let data = wide(&value);
        let rc = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                (data.len() * 2) as u32,
            )
        };
        unsafe { RegCloseKey(key) };
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
        Ok(true)
    }

    /// Drop a preference this tool wrote. A value carrying anything else (Auto
    /// HDR, a specific adapter, the user's own choice) is left alone.
    pub fn clear_ours(exe: &Path) -> std::io::Result<bool> {
        let Some(current) = get(exe) else {
            return Ok(false);
        };
        if current.trim() != HIGH_PERFORMANCE {
            return Ok(false);
        }
        let sub = wide(KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let rc =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_SET_VALUE, &mut key) };
        if rc != 0 {
            return Ok(false);
        }
        let name = wide(&exe.to_string_lossy());
        let rc = unsafe { RegDeleteValueW(key, name.as_ptr()) };
        unsafe { RegCloseKey(key) };
        Ok(rc == 0)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn get(_exe: &Path) -> Option<String> {
        None
    }
    pub fn set_high_performance(_exe: &Path) -> std::io::Result<bool> {
        Ok(false)
    }
    pub fn clear_ours(_exe: &Path) -> std::io::Result<bool> {
        Ok(false)
    }
}

pub use imp::{clear_ours, get, set_high_performance};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_token_is_replaced_not_appended() {
        assert_eq!(with_high_performance(""), "GpuPreference=2;");
        assert_eq!(
            with_high_performance("GpuPreference=1;"),
            "GpuPreference=2;"
        );
        // Auto HDR and the app-status flag Windows writes must survive.
        assert_eq!(
            with_high_performance("AppStatus=1;AutoHDREnable=2097;GpuPreference=0;"),
            "AppStatus=1;AutoHDREnable=2097;GpuPreference=2;"
        );
        assert!(is_high_performance("AutoHDREnable=2097;GpuPreference=2;"));
        assert!(!is_high_performance("GpuPreference=1;"));
        assert!(!is_high_performance(""));
    }

    /// The registry round trip, on a path that does not have to exist: write,
    /// read back, and remove again so the machine is left as it was found.
    #[cfg(windows)]
    #[test]
    fn registry_round_trip() {
        let exe = std::env::temp_dir().join("dlss5oneclick-gpupref-test.exe");
        assert!(get(&exe).is_none(), "left over from an earlier run");
        assert!(set_high_performance(&exe).unwrap());
        assert_eq!(get(&exe).as_deref(), Some(HIGH_PERFORMANCE));
        // Second call is a no-op, not a duplicate token.
        assert!(!set_high_performance(&exe).unwrap());
        assert!(clear_ours(&exe).unwrap());
        assert!(get(&exe).is_none());
    }
}
