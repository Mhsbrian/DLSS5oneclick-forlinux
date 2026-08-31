//! Which GPU is present, read from the display-adapter class key in the registry
//! (`DriverDesc` under `SYSTEM\CurrentControlSet\Control\Class\{4d36e968-…}\NNNN`).
//! No D3D device is created; this is enough to say "not NVIDIA" or "GTX, no tensor
//! cores" before anything is downloaded, and to show the expected cost tier.

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
    for i in 0..16 {
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

#[cfg(not(windows))]
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
}
