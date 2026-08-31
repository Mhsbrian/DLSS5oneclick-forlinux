//! Minimal ReShade .ini reader/writer.
//!
//! ReShade stores multi-values comma-separated and escapes a literal comma
//! as ",,". Key names verified against crosire/reshade source/runtime.cpp:
//! `[GENERAL] EffectSearchPaths / TextureSearchPaths / PreprocessorDefinitions /
//! PresetPath` in ReShade.ini; `Techniques`, `TechniqueSorting` and
//! `PreprocessorDefinitions` in the preset's root (section-less) block.
//! Technique entries are `Name@File.fx`.

use anyhow::Result;
use std::fs;
use std::path::Path;

pub const MV_PROVIDER_DEFINE: &str = "DLSS5_MV_PROVIDER=3"; // LumeniteFX Kernel
pub const TECHNIQUES_ORDERED: [&str; 2] = [
    "Lumenite_Kernel@lumenite_Kernel.fx", // provider must sit ABOVE the feed
    "DLSS5_Feed@DLSS5_Feed.fx",
];

/// Ordered sections; the first is always the root ("").
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Ini {
    pub sections: Vec<(String, Vec<(String, String)>)>,
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let mut ini = Ini {
            sections: vec![(String::new(), Vec::new())],
        };
        let mut cur = 0usize;
        for line in text.lines() {
            let s = line.trim();
            if s.is_empty() || s.starts_with(';') || s.starts_with('#') {
                continue;
            }
            if s.starts_with('[') && s.ends_with(']') {
                cur = ini.section_index(&s[1..s.len() - 1]);
                continue;
            }
            if let Some((k, v)) = s.split_once('=') {
                ini.sections[cur]
                    .1
                    .push((k.trim().to_owned(), v.trim().to_owned()));
            }
        }
        ini
    }

    fn section_index(&mut self, name: &str) -> usize {
        if let Some(i) = self
            .sections
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            return i;
        }
        self.sections.push((name.to_owned(), Vec::new()));
        self.sections.len() - 1
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(section))
            .and_then(|(_, kv)| kv.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)))
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, section: &str, key: &str, value: impl Into<String>) {
        let i = self.section_index(section);
        let kv = &mut self.sections[i].1;
        match kv.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            Some(e) => e.1 = value.into(),
            None => kv.push((key.to_owned(), value.into())),
        }
    }

    pub fn set_default(&mut self, section: &str, key: &str, value: &str) {
        if self.get(section, key).is_none() {
            self.set(section, key, value);
        }
    }

    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (i, (name, kv)) in self.sections.iter().enumerate() {
            if kv.is_empty() && name.is_empty() {
                continue;
            }
            if !name.is_empty() {
                if i > 0 && !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("[{name}]\n"));
            }
            for (k, v) in kv {
                out.push_str(&format!("{k}={v}\n"));
            }
        }
        out
    }

    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(t) => Ini::parse(&t),
            Err(_) => Ini::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write(path, self.dump())?;
        Ok(())
    }
}

/// Split on single commas; ",," is an escaped comma.
pub fn split_list(raw: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            if i + 1 < chars.len() && chars[i + 1] == ',' {
                cur.push(',');
                i += 2;
                continue;
            }
            items.push(std::mem::take(&mut cur));
        } else {
            cur.push(chars[i]);
        }
        i += 1;
    }
    if !cur.is_empty() {
        items.push(cur);
    }
    items.into_iter().filter(|s| !s.is_empty()).collect()
}

pub fn join_list(items: &[String]) -> String {
    items
        .iter()
        .map(|s| s.replace(',', ",,"))
        .collect::<Vec<_>>()
        .join(",")
}

fn ensure_define(raw: &str, define: &str) -> String {
    let name = define.split_once('=').map(|(n, _)| n).unwrap_or(define);
    let mut items: Vec<String> = split_list(raw)
        .into_iter()
        .filter(|d| d.split_once('=').map(|(n, _)| n).unwrap_or(d) != name)
        .collect();
    items.push(define.to_owned());
    join_list(&items)
}

/// Create/update ReShade.ini: search paths + PresetPath defaults, provider define forced.
pub fn write_reshade_ini(game_dir: &Path) -> Result<()> {
    let p = game_dir.join("ReShade.ini");
    let mut ini = Ini::load(&p);
    ini.set_default(
        "GENERAL",
        "EffectSearchPaths",
        r".\reshade-shaders\Shaders\**",
    );
    ini.set_default(
        "GENERAL",
        "TextureSearchPaths",
        r".\reshade-shaders\Textures\**",
    );
    ini.set_default("GENERAL", "PresetPath", r".\ReShadePreset.ini");
    let defs = ensure_define(
        ini.get("GENERAL", "PreprocessorDefinitions").unwrap_or(""),
        MV_PROVIDER_DEFINE,
    );
    ini.set("GENERAL", "PreprocessorDefinitions", defs);
    ini.save(&p)
}

/// Create/update ReShadePreset.ini: Lumenite_Kernel then DLSS5_Feed at the head of
/// the enabled list (existing user techniques kept after), provider define at preset level.
pub fn write_preset(game_dir: &Path) -> Result<()> {
    let p = game_dir.join("ReShadePreset.ini");
    let mut ini = Ini::load(&p);
    let ours: Vec<String> = TECHNIQUES_ORDERED.iter().map(|s| s.to_string()).collect();
    for key in ["Techniques", "TechniqueSorting"] {
        if key == "TechniqueSorting" && ini.get("", key).is_none() {
            continue;
        }
        let mut list = ours.clone();
        list.extend(
            split_list(ini.get("", key).unwrap_or(""))
                .into_iter()
                .filter(|t| !ours.contains(t)),
        );
        ini.set("", key, join_list(&list));
    }
    let defs = ensure_define(
        ini.get("", "PreprocessorDefinitions").unwrap_or(""),
        MV_PROVIDER_DEFINE,
    );
    ini.set("", "PreprocessorDefinitions", defs);
    ini.save(&p)
}

/// Drop Lumenite_Kernel / DLSS5_Feed from an existing preset (native-DLSS games do not use them).
pub fn remove_our_techniques(game_dir: &Path) -> Result<()> {
    let p = game_dir.join("ReShadePreset.ini");
    if !p.is_file() {
        return Ok(());
    }
    let mut ini = Ini::load(&p);
    for key in ["Techniques", "TechniqueSorting"] {
        if let Some(raw) = ini.get("", key) {
            let kept: Vec<String> = split_list(raw)
                .into_iter()
                .filter(|t| !TECHNIQUES_ORDERED.contains(&t.as_str()))
                .collect();
            ini.set("", key, join_list(&kept));
        }
    }
    ini.save(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_join_roundtrip_with_escaped_comma() {
        let raw = join_list(&["A=1".into(), "B=x,y".into(), "C".into()]);
        assert_eq!(raw, "A=1,B=x,,y,C");
        assert_eq!(split_list(&raw), vec!["A=1", "B=x,y", "C"]);
        assert!(split_list("").is_empty());
    }

    #[test]
    fn reshade_ini_fresh() {
        let t = tempfile::tempdir().unwrap();
        write_reshade_ini(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShade.ini"));
        assert_eq!(
            ini.get("GENERAL", "EffectSearchPaths"),
            Some(r".\reshade-shaders\Shaders\**")
        );
        assert_eq!(
            ini.get("GENERAL", "TextureSearchPaths"),
            Some(r".\reshade-shaders\Textures\**")
        );
        assert_eq!(
            ini.get("GENERAL", "PresetPath"),
            Some(r".\ReShadePreset.ini")
        );
        assert_eq!(
            ini.get("GENERAL", "PreprocessorDefinitions"),
            Some("DLSS5_MV_PROVIDER=3")
        );
    }

    #[test]
    fn reshade_ini_preserves_user_keys_and_replaces_define() {
        let t = tempfile::tempdir().unwrap();
        fs::write(
            t.path().join("ReShade.ini"),
            "[GENERAL]\nEffectSearchPaths=.\\custom\\**\nPreprocessorDefinitions=FOO=1,DLSS5_MV_PROVIDER=5\n[INPUT]\nKeyOverlay=36,0,0,0\n",
        )
        .unwrap();
        write_reshade_ini(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShade.ini"));
        assert_eq!(
            ini.get("GENERAL", "EffectSearchPaths"),
            Some(".\\custom\\**")
        );
        assert_eq!(
            split_list(ini.get("GENERAL", "PreprocessorDefinitions").unwrap()),
            vec!["FOO=1", "DLSS5_MV_PROVIDER=3"]
        );
        assert_eq!(ini.get("INPUT", "KeyOverlay"), Some("36,0,0,0"));
    }

    #[test]
    fn remove_our_techniques_keeps_user_ones() {
        let t = tempfile::tempdir().unwrap();
        write_preset(t.path()).unwrap();
        let p = t.path().join("ReShadePreset.ini");
        let mut ini = Ini::load(&p);
        ini.set(
            "",
            "Techniques",
            "Lumenite_Kernel@lumenite_Kernel.fx,DLSS5_Feed@DLSS5_Feed.fx,Clarity@Clarity.fx",
        );
        ini.save(&p).unwrap();
        remove_our_techniques(t.path()).unwrap();
        assert_eq!(
            Ini::load(&p).get("", "Techniques"),
            Some("Clarity@Clarity.fx")
        );
    }

    #[test]
    fn preset_fresh() {
        let t = tempfile::tempdir().unwrap();
        write_preset(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShadePreset.ini"));
        assert_eq!(
            split_list(ini.get("", "Techniques").unwrap()),
            TECHNIQUES_ORDERED
        );
        assert_eq!(
            ini.get("", "PreprocessorDefinitions"),
            Some("DLSS5_MV_PROVIDER=3")
        );
        assert!(ini.get("", "TechniqueSorting").is_none());
    }

    #[test]
    fn preset_keeps_provider_above_feed_and_user_techniques() {
        let t = tempfile::tempdir().unwrap();
        fs::write(
            t.path().join("ReShadePreset.ini"),
            "Techniques=DLSS5_Feed@DLSS5_Feed.fx,Clarity@Clarity.fx\nTechniqueSorting=Clarity@Clarity.fx,DLSS5_Feed@DLSS5_Feed.fx\n[Clarity.fx]\nStrength=0.5\n",
        )
        .unwrap();
        write_preset(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShadePreset.ini"));
        assert_eq!(
            split_list(ini.get("", "Techniques").unwrap()),
            vec![
                "Lumenite_Kernel@lumenite_Kernel.fx",
                "DLSS5_Feed@DLSS5_Feed.fx",
                "Clarity@Clarity.fx"
            ]
        );
        assert_eq!(
            &split_list(ini.get("", "TechniqueSorting").unwrap())[..2],
            TECHNIQUES_ORDERED
        );
        assert_eq!(ini.get("Clarity.fx", "Strength"), Some("0.5"));
        let text = fs::read_to_string(t.path().join("ReShadePreset.ini")).unwrap();
        assert!(
            text.starts_with("Techniques="),
            "root keys must precede sections: {text}"
        );
    }
}
