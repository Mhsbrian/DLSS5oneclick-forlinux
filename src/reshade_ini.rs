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

pub const MV_PROVIDER_DEFINE: &str = "DLSS5_MV_PROVIDER=3"; // LumeniteFX Kernel (also used when OFA replaces MV at runtime)
pub const TECHNIQUE_LUMENITE: &str = "Lumenite_Kernel@lumenite_Kernel.fx";
pub const TECHNIQUE_FEED: &str = "DLSS5_Feed@DLSS5_Feed.fx";
pub const TECHNIQUES_ORDERED: [&str; 2] = [TECHNIQUE_LUMENITE, TECHNIQUE_FEED];

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

/// ReShade recursive glob is a single trailing `\**`. A doubled `\**\**` (common after
/// ReShade's path UI rewrites our default) fails Win32 resolve with ERROR_INVALID_NAME
/// (123) and the overlay reports "No effect files (.fx) found".
fn normalize_search_path(raw: &str) -> String {
    let mut s = raw.trim().replace('/', "\\");
    while s.contains(r"**\**") {
        s = s.replace(r"**\**", "**");
    }
    s
}

fn path_key(p: &str) -> String {
    normalize_search_path(p)
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

/// Collapse broken `\**\**` globs and ensure `required` is present (prepended if missing).
fn ensure_search_paths(raw: &str, required: &str) -> String {
    let required = normalize_search_path(required);
    let req_key = path_key(&required);
    let mut items: Vec<String> = split_list(raw)
        .into_iter()
        .map(|p| normalize_search_path(&p))
        .filter(|p| !p.is_empty())
        .collect();
    if !items.iter().any(|p| path_key(p) == req_key) {
        items.insert(0, required);
    }
    join_list(&items)
}

pub const EFFECT_SEARCH_PATH: &str = r".\reshade-shaders\Shaders\**";
pub const TEXTURE_SEARCH_PATH: &str = r".\reshade-shaders\Textures\**";

/// Create/update ReShade.ini: search paths + PresetPath defaults, provider define forced.
pub fn write_reshade_ini(game_dir: &Path) -> Result<()> {
    let p = game_dir.join("ReShade.ini");
    let mut ini = Ini::load(&p);
    // Always rewrite search paths: set_default left broken `\**\**` values from ReShade
    // alone, which makes Install look successful while the overlay finds zero .fx files.
    ini.set(
        "GENERAL",
        "EffectSearchPaths",
        ensure_search_paths(
            ini.get("GENERAL", "EffectSearchPaths").unwrap_or(""),
            EFFECT_SEARCH_PATH,
        ),
    );
    ini.set(
        "GENERAL",
        "TextureSearchPaths",
        ensure_search_paths(
            ini.get("GENERAL", "TextureSearchPaths").unwrap_or(""),
            TEXTURE_SEARCH_PATH,
        ),
    );
    ini.set_default("GENERAL", "PresetPath", r".\ReShadePreset.ini");
    let defs = ensure_define(
        ini.get("GENERAL", "PreprocessorDefinitions").unwrap_or(""),
        MV_PROVIDER_DEFINE,
    );
    ini.set("GENERAL", "PreprocessorDefinitions", defs);
    ini.save(&p)
}

/// Create/update ReShadePreset.ini. When `enable_lumenite` is false (Optical Flow preset),
/// only DLSS5_Feed is enabled; Lumenite files may still be on disk as fallback.
pub fn write_preset(game_dir: &Path, enable_lumenite: bool) -> Result<()> {
    let p = game_dir.join("ReShadePreset.ini");
    let mut ini = Ini::load(&p);
    let ours: Vec<String> = if enable_lumenite {
        TECHNIQUES_ORDERED.iter().map(|s| s.to_string()).collect()
    } else {
        vec![TECHNIQUE_FEED.to_string()]
    };
    // Drop both of our techniques from the existing list, then prepend the desired set.
    let drop: &[&str] = &TECHNIQUES_ORDERED;
    for key in ["Techniques", "TechniqueSorting"] {
        if key == "TechniqueSorting" && ini.get("", key).is_none() {
            continue;
        }
        let mut list = ours.clone();
        list.extend(
            split_list(ini.get("", key).unwrap_or(""))
                .into_iter()
                .filter(|t| !drop.iter().any(|d| d == t)),
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

/// Write residual-mask uniforms into `[DLSS5_Feed.fx]`.
pub fn write_feed_fx_uniforms(game_dir: &Path, kv: &[(&str, String)]) -> Result<()> {
    let p = game_dir.join("ReShadePreset.ini");
    let mut ini = Ini::load(&p);
    for (k, v) in kv {
        ini.set("DLSS5_Feed.fx", k, v.clone());
    }
    ini.save(&p)
}

/// Soft defaults for optional LUMENITE: TRAA (does not enable the technique).
/// Geometric DLAA + UI protect reduce HUD/text smear when the user turns TRAA on.
pub fn write_traa_ui_defaults(game_dir: &Path) -> Result<()> {
    let p = game_dir.join("ReShadePreset.ini");
    if !p.is_file() {
        return Ok(());
    }
    let mut ini = Ini::load(&p);
    let section = "lumenite_TRAA.fx";
    ini.set_default(section, "EDGE_MODE", "1");
    ini.set_default(section, "UI_PROTECT", "1");
    ini.set_default(section, "UI_PROTECT_STRENGTH", "1.000000");
    ini.set_default(section, "SHARP_STRENGTH", "0.700000");
    ini.save(&p)
}

/// neural-upstream's strength presets, exactly as its own `apply_preset()`
/// defines them: `(label, id, [intensity, local tone, local structure, skin
/// structure])`. Skin structure -1 means "follow local structure".
pub const UPSTREAM_PRESETS: [(&str, u8, [f32; 4]); 5] = [
    ("Light", 1, [0.45, 0.55, 0.25, 0.15]),
    ("Moderate", 2, [0.70, 0.80, 0.55, 0.40]),
    ("Reference", 3, [1.00, 1.00, 1.00, -1.00]),
    ("Overdrive", 4, [1.30, 1.25, 1.45, 1.20]),
    ("AI slop", 5, [1.80, 1.60, 2.00, 1.90]),
];

/// Seeds neural-upstream's strength preset before the game starts (#68).
///
/// The add-on reads its settings from `ReShade.ini`'s `[NRPreUpscale]` section
/// through `reshade::get_config_value`, so they can be chosen from here rather
/// than only in the in-game overlay. It reads `Preset` for the label and the
/// four strength values as separate keys, and does **not** derive one from the
/// other, so both go in — with the values its own `apply_preset()` would set.
pub fn write_upstream_preset(game_dir: &Path, preset: u8) -> Result<()> {
    let Some((_, id, v)) = UPSTREAM_PRESETS.iter().find(|(_, id, _)| *id == preset) else {
        return Ok(()); // 0 = custom: leave whatever the user set in the overlay
    };
    let p = game_dir.join("ReShade.ini");
    let mut ini = Ini::load(&p);
    ini.set("NRPreUpscale", "Preset", id.to_string());
    for (key, val) in [
        ("Intensity", v[0]),
        ("LocalTone", v[1]),
        ("LocalStructure", v[2]),
        ("SkinStructure", v[3]),
    ] {
        ini.set("NRPreUpscale", key, format!("{val:.6}"));
    }
    ini.save(&p)
}

/// ReShade keeps a per-game `[ADDON] DisabledAddons=` list (entries are
/// `Name`, `Name@file` or `@file`). A stray disable hides the DLSS 5 panel, so
/// an install drops our add-ons from that list.
pub fn clear_disabled_addons(game_dir: &Path) -> Result<()> {
    let p = game_dir.join("ReShade.ini");
    if !p.is_file() {
        return Ok(());
    }
    let mut ini = Ini::load(&p);
    let Some(raw) = ini.get("ADDON", "DisabledAddons") else {
        return Ok(());
    };
    let ours_files = [
        "renodx-dlss5.addon64",
        "dlss5-feed.addon64",
        "dlss5-bridge.addon64",
        "dlss5-dx11-bridge.addon64",
    ];
    let kept: Vec<String> = split_list(raw)
        .into_iter()
        .filter(|e| {
            let (name, file) = match e.split_once('@') {
                Some((n, f)) => (n, f),
                None => (e.as_str(), ""),
            };
            let file_ours = ours_files.iter().any(|f| f.eq_ignore_ascii_case(file));
            let name_ours = name.to_ascii_lowercase().starts_with("dlss 5");
            !(file_ours || name_ours)
        })
        .collect();
    ini.set("ADDON", "DisabledAddons", join_list(&kept));
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

    /// The preset has to reach the add-on as both the label and the four
    /// values: neural-upstream reads them as separate keys and never derives
    /// one from the other, so writing `Preset` alone would show "Light" in the
    /// overlay while the network still ran at Reference strength (#68).
    #[test]
    fn upstream_preset_writes_label_and_values() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join("ReShade.ini"),
            "[GENERAL]\nEffectSearchPaths=.\\reshade-shaders\\Shaders\\**\n",
        )
        .unwrap();
        write_upstream_preset(t.path(), 1).unwrap();
        let out = std::fs::read_to_string(t.path().join("ReShade.ini")).unwrap();
        let ini = Ini::parse(&out);
        assert_eq!(ini.get("NRPreUpscale", "Preset"), Some("1"));
        assert_eq!(ini.get("NRPreUpscale", "Intensity"), Some("0.450000"));
        assert_eq!(ini.get("NRPreUpscale", "LocalTone"), Some("0.550000"));
        assert_eq!(ini.get("NRPreUpscale", "LocalStructure"), Some("0.250000"));
        assert_eq!(ini.get("NRPreUpscale", "SkinStructure"), Some("0.150000"));
        // Whatever else was in the file is still there.
        assert!(ini.get("GENERAL", "EffectSearchPaths").is_some(), "{out}");

        // Reference keeps the add-on's "follow local structure" sentinel.
        write_upstream_preset(t.path(), 3).unwrap();
        let out = std::fs::read_to_string(t.path().join("ReShade.ini")).unwrap();
        let ini = Ini::parse(&out);
        assert_eq!(ini.get("NRPreUpscale", "Preset"), Some("3"));
        assert_eq!(ini.get("NRPreUpscale", "SkinStructure"), Some("-1.000000"));

        // 0 is "custom": the overlay's own settings are left alone.
        write_upstream_preset(t.path(), 0).unwrap();
        let ini = Ini::parse(&std::fs::read_to_string(t.path().join("ReShade.ini")).unwrap());
        assert_eq!(ini.get("NRPreUpscale", "Preset"), Some("3"));
    }
    use super::*;

    #[test]
    fn split_join_roundtrip_with_escaped_comma() {
        let raw = join_list(&["A=1".into(), "B=x,y".into(), "C".into()]);
        assert_eq!(raw, "A=1,B=x,,y,C");
        assert_eq!(split_list(&raw), vec!["A=1", "B=x,y", "C"]);
        assert!(split_list("").is_empty());
    }

    /// A game that already had ReShade kept its own search paths and never saw
    /// the shaders this tool installed; the reporter had to copy them by hand
    /// (#4). Ours is now appended to whatever is already there.
    #[test]
    fn existing_search_paths_keep_theirs_and_gain_ours() {
        let t = tempfile::tempdir().unwrap();
        fs::write(
            t.path().join("ReShade.ini"),
            "[GENERAL]\nEffectSearchPaths=D:\\my shaders\\**\nTextureSearchPaths=D:\\my textures\n",
        )
        .unwrap();
        write_reshade_ini(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShade.ini"));
        let fx = ini.get("GENERAL", "EffectSearchPaths").unwrap().to_owned();
        assert!(fx.contains(r"D:\my shaders\**"), "{fx}");
        assert!(fx.contains(r".\reshade-shaders\Shaders\**"), "{fx}");
        let tx = ini.get("GENERAL", "TextureSearchPaths").unwrap().to_owned();
        assert!(tx.contains(r"D:\my textures"), "{tx}");
        assert!(tx.contains(r".\reshade-shaders\Textures\**"), "{tx}");

        // Running it twice must not duplicate our entry.
        write_reshade_ini(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShade.ini"));
        let fx = ini.get("GENERAL", "EffectSearchPaths").unwrap();
        assert_eq!(
            fx.matches(r".\reshade-shaders\Shaders\**").count(),
            1,
            "{fx}"
        );
    }

    #[test]
    fn reshade_ini_fresh() {
        let t = tempfile::tempdir().unwrap();
        write_reshade_ini(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShade.ini"));
        assert_eq!(
            ini.get("GENERAL", "EffectSearchPaths"),
            Some(EFFECT_SEARCH_PATH)
        );
        assert_eq!(
            ini.get("GENERAL", "TextureSearchPaths"),
            Some(TEXTURE_SEARCH_PATH)
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
        // The user's own path stays, and ours joins it — keeping only theirs
        // meant the shaders this tool installs were never found (#4).
        assert_eq!(
            split_list(ini.get("GENERAL", "EffectSearchPaths").unwrap()),
            vec![EFFECT_SEARCH_PATH, r".\custom\**"]
        );
        assert_eq!(
            split_list(ini.get("GENERAL", "PreprocessorDefinitions").unwrap()),
            vec!["FOO=1", "DLSS5_MV_PROVIDER=3"]
        );
        assert_eq!(ini.get("INPUT", "KeyOverlay"), Some("36,0,0,0"));
    }

    #[test]
    fn reshade_ini_collapses_doubled_recursive_glob() {
        let t = tempfile::tempdir().unwrap();
        fs::write(
            t.path().join("ReShade.ini"),
            "[GENERAL]\nEffectSearchPaths=.\\reshade-shaders\\Shaders\\**\\**\nTextureSearchPaths=.\\reshade-shaders\\Textures\\**\\**\n",
        )
        .unwrap();
        write_reshade_ini(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShade.ini"));
        assert_eq!(
            ini.get("GENERAL", "EffectSearchPaths"),
            Some(EFFECT_SEARCH_PATH)
        );
        assert_eq!(
            ini.get("GENERAL", "TextureSearchPaths"),
            Some(TEXTURE_SEARCH_PATH)
        );
    }

    #[test]
    fn remove_our_techniques_keeps_user_ones() {
        let t = tempfile::tempdir().unwrap();
        write_preset(t.path(), true).unwrap();
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
        write_preset(t.path(), true).unwrap();
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
        write_preset(t.path(), true).unwrap();
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

    #[test]
    fn traa_ui_defaults_are_soft() {
        let t = tempfile::tempdir().unwrap();
        fs::write(
            t.path().join("ReShadePreset.ini"),
            "Techniques=DLSS5_Feed@DLSS5_Feed.fx\n[lumenite_TRAA.fx]\nEDGE_MODE=0\nSHARP_STRENGTH=1.000000\n",
        )
        .unwrap();
        write_traa_ui_defaults(t.path()).unwrap();
        let ini = Ini::load(&t.path().join("ReShadePreset.ini"));
        // Existing EDGE_MODE kept; missing UI_PROTECT filled in.
        assert_eq!(ini.get("lumenite_TRAA.fx", "EDGE_MODE"), Some("0"));
        assert_eq!(ini.get("lumenite_TRAA.fx", "UI_PROTECT"), Some("1"));
        assert_eq!(
            ini.get("lumenite_TRAA.fx", "SHARP_STRENGTH"),
            Some("1.000000")
        );
    }
}
