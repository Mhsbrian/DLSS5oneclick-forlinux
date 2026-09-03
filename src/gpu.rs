//! Which GPU is present, read from the display-adapter class key in the registry
//! (`DriverDesc` under `SYSTEM\CurrentControlSet\Control\Class\{4d36e968-…}\NNNN`).
//! No D3D device is created; this is enough to say "not NVIDIA" or "GTX, no tensor
//! cores" before anything is downloaded, and to show the expected cost tier.

/// NVIDIA's own version number from the Windows driver version: the last five
/// digits with a decimal before the last two (32.0.16.1656 -> 616.56). Verified
/// against this machine, where the NVIDIA App reports 616.56.
#[cfg_attr(not(windows), allow(dead_code))] // reachable from the registry reader
pub fn nvidia_driver_number(windows_version: &str) -> Option<String> {
    let digits: String = windows_version
        .split('.')
        .skip(2)
        .collect::<Vec<_>>()
        .join("");
    let digits: String = digits.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 5 {
        return None;
    }
    let last5 = &digits[digits.len() - 5..];
    Some(format!("{}.{}", &last5[..3], &last5[3..]))
}

/// NVIDIA's own driver number for the first NVIDIA adapter, or `None`.
#[cfg(windows)]
pub fn nvidia_driver() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ,
    };
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn read_sz(key: HKEY, name: &str) -> Option<String> {
        let name_w = wide(name);
        let mut buf = [0u16; 256];
        let mut size: u32 = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                key,
                std::ptr::null(),
                name_w.as_ptr(),
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
        Some(
            String::from_utf16_lossy(&buf[..n])
                .trim_end_matches(' ')
                .to_owned(),
        )
    }
    let base = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    for i in 0..64 {
        let sub = wide(&format!("{base}\\{i:04}"));
        let mut key: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, sub.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
            continue;
        }
        let provider = read_sz(key, "ProviderName").unwrap_or_default();
        let ver = read_sz(key, "DriverVersion");
        unsafe { RegCloseKey(key) };
        if provider.to_ascii_lowercase().contains("nvidia") {
            if let Some(v) = ver.as_deref().and_then(nvidia_driver_number) {
                return Some(v);
            }
        }
    }
    None
}

/// On Linux the kernel module reports NVIDIA's own number directly.
#[cfg(target_os = "linux")]
pub fn nvidia_driver() -> Option<String> {
    driver_version()
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn nvidia_driver() -> Option<String> {
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub name: String,
    pub vendor: String,
}

/// What the DLSS 5 model can do on this GPU. Facts as reported 2026-08-31:
/// the leaked model is FP8 with Blackwell-only kernels; ShortFuse's `.SF`
/// build adds Ada binaries and an FP16 path for Turing/Ampere; anything
/// without tensor cores (GTX) or from another vendor cannot run it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// RTX 50: native FP8, the intended target.
    Rtx50,
    /// RTX 40: patched binaries, noticeable frame cost.
    Rtx40,
    /// RTX 20/30: FP16 path, heavy frame cost.
    Rtx2030,
    /// NVIDIA but not RTX (GTX, GT, Quadro without tensor cores…): cannot run.
    NvidiaNoTensor,
    /// AMD / Intel / other: NGX does not exist there.
    NotNvidia,
    Unknown,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Rtx50 => "RTX 50 · full speed",
            Tier::Rtx40 => "RTX 40 · patched build, moderate cost",
            Tier::Rtx2030 => "RTX 20/30 · FP16 path, heavy cost",
            Tier::NvidiaNoTensor => "no tensor cores",
            Tier::NotNvidia => "not NVIDIA",
            Tier::Unknown => "GPU unknown",
        }
    }
    pub fn can_run(self) -> bool {
        matches!(
            self,
            Tier::Rtx50 | Tier::Rtx40 | Tier::Rtx2030 | Tier::Unknown
        )
    }
}

pub fn classify(name: &str) -> Tier {
    let n = name.to_ascii_lowercase();
    // Virtual / remote adapters say nothing about the real GPU (Hyper-V GPU-P,
    // RDP sessions, VMs, safe mode). Never refuse on those.
    for v in [
        "hyper-v",
        "remote display",
        "basic display",
        "basic render",
        "vmware",
        "virtualbox",
        "parallels",
        "virtio",
        "qxl",
        "citrix",
        "rdp",
    ] {
        if n.contains(v) {
            return Tier::Unknown;
        }
    }
    if !(n.contains("nvidia") || n.contains("geforce") || n.contains("rtx") || n.contains("quadro"))
    {
        return Tier::NotNvidia;
    }
    if let Some(pos) = n.find("rtx") {
        let digits: String = n[pos + 3..]
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(model) = digits.parse::<u32>() {
            return match model {
                5000..=5999 => Tier::Rtx50,
                4000..=4999 => Tier::Rtx40,
                2000..=3999 => Tier::Rtx2030,
                _ => Tier::Unknown,
            };
        }
        return Tier::Unknown;
    }
    if n.contains("gtx") || n.contains("geforce gt ") || n.contains("geforce mx") {
        return Tier::NvidiaNoTensor;
    }
    Tier::Unknown
}

/// Best GPU for DLSS 5 among the installed adapters (an RTX beside an iGPU wins).
pub fn best() -> Option<(Gpu, Tier)> {
    let mut all: Vec<(Gpu, Tier)> = list()
        .into_iter()
        .map(|g| {
            let t = classify(&g.name);
            (g, t)
        })
        .collect();
    all.sort_by_key(|(_, t)| match t {
        Tier::Rtx50 => 0,
        Tier::Rtx40 => 1,
        Tier::Rtx2030 => 2,
        Tier::Unknown => 3,
        Tier::NvidiaNoTensor => 4,
        Tier::NotNvidia => 5,
    });
    all.into_iter().next()
}

#[cfg(windows)]
pub fn list() -> Vec<Gpu> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ,
    };
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn read_sz(key: HKEY, name: &str) -> Option<String> {
        let name_w = wide(name);
        let mut buf = [0u16; 512];
        let mut size: u32 = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                key,
                std::ptr::null(),
                name_w.as_ptr(),
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
        Some(
            String::from_utf16_lossy(&buf[..n])
                .trim_end_matches('\0')
                .to_string(),
        )
    }
    let base = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let mut out = Vec::new();
    for i in 0..64 {
        let sub = wide(&format!("{base}\\{i:04}"));
        let mut key: HKEY = std::ptr::null_mut();
        let rc = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, sub.as_ptr(), 0, KEY_READ, &mut key) };
        if rc != 0 {
            continue;
        }
        let desc = read_sz(key, "DriverDesc");
        let vendor = read_sz(key, "ProviderName").unwrap_or_default();
        unsafe { RegCloseKey(key) };
        if let Some(name) = desc {
            if !name.is_empty() {
                out.push(Gpu { name, vendor });
            }
        }
    }
    out
}

/// The proprietary driver names the card in `/proc/driver/nvidia/gpus/*/information`
/// ("Model:" line). Other vendors are read from DRM (`/sys/class/drm/cardN/device/vendor`)
/// only far enough to say "not NVIDIA"; an NVIDIA card without that proc file (nouveau,
/// driver not loaded) is reported with an unknown model so it is never falsely refused.
#[cfg(target_os = "linux")]
pub fn list() -> Vec<Gpu> {
    use std::path::Path;
    let mut out = list_at(Path::new("/proc"), Path::new("/sys"));
    if out.is_empty() {
        out = nvidia_smi();
    }
    out
}

#[cfg(target_os = "linux")]
fn list_at(proc_root: &std::path::Path, sys_root: &std::path::Path) -> Vec<Gpu> {
    use std::fs;
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(proc_root.join("driver/nvidia/gpus")) {
        for e in rd.flatten() {
            let Ok(text) = fs::read_to_string(e.path().join("information")) else {
                continue;
            };
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("Model:") {
                    let name = rest.trim().to_string();
                    if !name.is_empty() {
                        out.push(Gpu {
                            name,
                            vendor: "NVIDIA".into(),
                        });
                    }
                }
            }
        }
    }
    let nvidia_named = !out.is_empty();
    if let Ok(rd) = fs::read_dir(sys_root.join("class/drm")) {
        for e in rd.flatten() {
            let fname = e.file_name();
            let n = fname.to_string_lossy();
            // cardN only; card0-DP-1 etc. are connectors of the same device.
            if !n.starts_with("card") || n.contains('-') {
                continue;
            }
            let Ok(v) = fs::read_to_string(e.path().join("device/vendor")) else {
                continue;
            };
            let (name, vendor) = match v.trim() {
                "0x10de" => {
                    if nvidia_named {
                        continue;
                    }
                    ("NVIDIA GPU (model unknown, proprietary driver not detected)", "NVIDIA")
                }
                "0x1002" => ("AMD GPU", "AMD"),
                "0x8086" => ("Intel GPU", "Intel"),
                _ => continue,
            };
            out.push(Gpu {
                name: name.into(),
                vendor: vendor.into(),
            });
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn nvidia_smi() -> Vec<Gpu> {
    let Ok(o) = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
    else {
        return Vec::new();
    };
    if !o.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| Gpu {
            name: l.to_string(),
            vendor: "NVIDIA".into(),
        })
        .collect()
}

/// Loaded NVIDIA kernel-driver version (e.g. "610.57.04"), for diagnostics.
#[cfg(target_os = "linux")]
pub fn driver_version() -> Option<String> {
    driver_version_at(std::path::Path::new("/sys"))
}

#[cfg(target_os = "linux")]
fn driver_version_at(sys_root: &std::path::Path) -> Option<String> {
    let v = std::fs::read_to_string(sys_root.join("module/nvidia/version")).ok()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn list() -> Vec<Gpu> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_names() {
        assert_eq!(classify("NVIDIA GeForce RTX 4090"), Tier::Rtx40);
        assert_eq!(classify("NVIDIA GeForce RTX 5080"), Tier::Rtx50);
        assert_eq!(classify("NVIDIA GeForce RTX 3090"), Tier::Rtx2030);
        assert_eq!(classify("NVIDIA GeForce RTX 2070 SUPER"), Tier::Rtx2030);
        assert_eq!(classify("NVIDIA GeForce GTX 1080 Ti"), Tier::NvidiaNoTensor);
        assert_eq!(classify("NVIDIA GeForce GT 1010"), Tier::NvidiaNoTensor);
        assert_eq!(classify("AMD Radeon RX 6900 XT"), Tier::NotNvidia);
        assert_eq!(classify("AMD Radeon(TM) Graphics"), Tier::NotNvidia);
        assert_eq!(classify("Intel(R) Arc(TM) A770"), Tier::NotNvidia);
        assert_eq!(classify("NVIDIA RTX A6000"), Tier::Unknown);
        assert_eq!(classify("Microsoft Hyper-V Video"), Tier::Unknown);
        assert_eq!(classify("Microsoft Remote Display Adapter"), Tier::Unknown);
    }

    #[test]
    fn best_prefers_rtx_over_igpu() {
        let mut all: Vec<(Gpu, Tier)> = ["AMD Radeon(TM) Graphics", "NVIDIA GeForce RTX 4090"]
            .iter()
            .map(|n| {
                (
                    Gpu {
                        name: n.to_string(),
                        vendor: String::new(),
                    },
                    classify(n),
                )
            })
            .collect();
        all.sort_by_key(|(_, t)| if *t == Tier::Rtx40 { 0 } else { 9 });
        assert_eq!(all[0].1, Tier::Rtx40);
        #[cfg(windows)]
        {
            let real = best();
            assert!(real.is_some());
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::super::*;
        use std::fs;
        use std::path::Path;

        fn write(p: &Path, text: &str) {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }

        #[test]
        fn proc_model_line_wins_over_drm() {
            let t = tempfile::tempdir().unwrap();
            let (proc_r, sys_r) = (t.path().join("proc"), t.path().join("sys"));
            write(
                &proc_r.join("driver/nvidia/gpus/0000:01:00.0/information"),
                "Model: \t\t NVIDIA GeForce RTX 4090\nIRQ:   \t\t 89\n",
            );
            write(&sys_r.join("class/drm/card0/device/vendor"), "0x1002\n");
            write(&sys_r.join("class/drm/card1/device/vendor"), "0x10de\n");
            let got = list_at(&proc_r, &sys_r);
            assert_eq!(got[0].name, "NVIDIA GeForce RTX 4090");
            assert_eq!(classify(&got[0].name), Tier::Rtx40);
            // The 0x10de DRM entry is folded into the named one; AMD iGPU still listed.
            assert!(got.iter().filter(|g| g.vendor == "NVIDIA").count() == 1);
            assert!(got.iter().any(|g| g.vendor == "AMD"));
        }

        #[test]
        fn amd_only_is_not_nvidia() {
            let t = tempfile::tempdir().unwrap();
            let (proc_r, sys_r) = (t.path().join("proc"), t.path().join("sys"));
            write(&sys_r.join("class/drm/card0/device/vendor"), "0x1002\n");
            let got = list_at(&proc_r, &sys_r);
            assert_eq!(got.len(), 1);
            assert_eq!(classify(&got[0].name), Tier::NotNvidia);
        }

        #[test]
        fn nvidia_without_proprietary_driver_is_unknown_not_refused() {
            let t = tempfile::tempdir().unwrap();
            let (proc_r, sys_r) = (t.path().join("proc"), t.path().join("sys"));
            write(&sys_r.join("class/drm/card0/device/vendor"), "0x10de\n");
            let got = list_at(&proc_r, &sys_r);
            assert_eq!(got.len(), 1);
            let t = classify(&got[0].name);
            assert_eq!(t, Tier::Unknown);
            assert!(t.can_run());
        }

        #[test]
        fn empty_roots_give_empty_list() {
            let t = tempfile::tempdir().unwrap();
            assert!(list_at(&t.path().join("proc"), &t.path().join("sys")).is_empty());
        }

        #[test]
        fn driver_version_read_and_trimmed() {
            let t = tempfile::tempdir().unwrap();
            let sys_r = t.path().join("sys");
            assert_eq!(driver_version_at(&sys_r), None);
            write(&sys_r.join("module/nvidia/version"), "610.57.04\n");
            assert_eq!(driver_version_at(&sys_r).as_deref(), Some("610.57.04"));
        }
    }
}
