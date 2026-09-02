//! Heroic Games Launcher (Epic + GOG): enumerate installed games and set the
//! per-game environment variables the install needs.
//!
//! Installed games are plain JSON: `legendaryConfig/legendary/installed.json`
//! (Epic, an object keyed by app_name) and `gog_store/installed.json` (an
//! `{"installed": […]}` array). Per-game settings live in
//! `GamesConfig/<app_name>.json`; the env array key has been spelled
//! `enviromentOptions` (sic) by Heroic for years, with `environmentOptions` /
//! `environmentVariables` seen in other builds — all three are accepted, and
//! a file without any gains the historical spelling. Unknown keys are never
//! touched. Marked experimental in the README until verified against a live
//! Heroic install.

use super::launch_options::LaunchReq;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeroicGame {
    pub name: String,
    pub app_name: String,
    pub dir: PathBuf,
    pub store: &'static str, // "epic" | "gog"
}

#[cfg(target_os = "linux")]
pub fn roots() -> Vec<PathBuf> {
    match std::env::var_os("HOME") {
        Some(h) => roots_from(Path::new(&h)),
        None => Vec::new(),
    }
}

pub fn roots_from(home: &Path) -> Vec<PathBuf> {
    [
        home.join(".config/heroic"),
        home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"),
    ]
    .into_iter()
    .filter(|p| p.is_dir())
    .collect()
}

fn str_of<'a>(obj: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| obj.get(*k).and_then(Value::as_str))
}

pub fn games(root: &Path) -> Vec<HeroicGame> {
    let mut out = Vec::new();
    // Epic via legendary: { "<app_name>": { "title", "install_path", … }, … }
    if let Ok(text) = fs::read_to_string(root.join("legendaryConfig/legendary/installed.json")) {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
            for (app_name, v) in map {
                let Some(obj) = v.as_object() else { continue };
                let Some(dir) = str_of(obj, &["install_path"]) else {
                    continue;
                };
                let dir = PathBuf::from(dir);
                if !dir.is_dir() {
                    continue;
                }
                out.push(HeroicGame {
                    name: str_of(obj, &["title"]).unwrap_or(&app_name).to_string(),
                    app_name,
                    dir,
                    store: "epic",
                });
            }
        }
    }
    // GOG: { "installed": [ { "appName", "install_path", … } ] }
    if let Ok(text) = fs::read_to_string(root.join("gog_store/installed.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(arr) = v.get("installed").and_then(Value::as_array) {
                for g in arr {
                    let Some(obj) = g.as_object() else { continue };
                    let Some(app_name) = str_of(obj, &["appName", "app_name"]) else {
                        continue;
                    };
                    let Some(dir) = str_of(obj, &["install_path"]) else {
                        continue;
                    };
                    let dir = PathBuf::from(dir);
                    if !dir.is_dir() {
                        continue;
                    }
                    out.push(HeroicGame {
                        name: str_of(obj, &["title"]).unwrap_or(app_name).to_string(),
                        app_name: app_name.to_string(),
                        dir,
                        store: "gog",
                    });
                }
            }
        }
    }
    out.sort_by_key(|g| g.name.to_lowercase());
    out
}

const ENV_KEYS: [&str; 3] = [
    "enviromentOptions", // Heroic's own historical spelling
    "environmentOptions",
    "environmentVariables",
];

/// Set the required env vars in `GamesConfig/<app_name>.json`. The file must
/// already exist (Heroic creates it when the game's settings are first opened);
/// otherwise the caller shows the copy-paste instructions instead.
pub fn apply_env(root: &Path, app_name: &str, req: &LaunchReq) -> Result<PathBuf> {
    let file = root.join("GamesConfig").join(format!("{app_name}.json"));
    let text = fs::read_to_string(&file).with_context(|| {
        format!(
            "Heroic has no settings file for this game yet ({}); open the game's \
             settings in Heroic once, or add the variables there yourself",
            file.display()
        )
    })?;
    let mut doc: Value = serde_json::from_str(&text)
        .with_context(|| format!("cannot parse {}", file.display()))?;
    let Some(top) = doc.as_object_mut() else {
        bail!("{} is not a JSON object", file.display());
    };
    // The game's settings object: its own key, else the only non-"version" one.
    let game_key = if top.contains_key(app_name) {
        app_name.to_string()
    } else {
        let mut candidates = top.keys().filter(|k| *k != "version").cloned();
        match (candidates.next(), candidates.next()) {
            (Some(k), None) => k,
            _ => bail!("cannot identify the game object in {}", file.display()),
        }
    };
    let game = top
        .get_mut(&game_key)
        .and_then(Value::as_object_mut)
        .with_context(|| format!("game entry in {} is not an object", file.display()))?;
    let key = ENV_KEYS
        .iter()
        .find(|k| game.contains_key(**k))
        .copied()
        .unwrap_or(ENV_KEYS[0]);
    let arr = game
        .entry(key)
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .with_context(|| format!("{key} in {} is not an array", file.display()))?;

    for (name, value) in super::launch_options::env_pairs(req) {
        let existing = arr.iter_mut().find(|e| {
            e.get("key").and_then(Value::as_str) == Some(name.as_str())
        });
        match existing {
            Some(e) => {
                if let Some(obj) = e.as_object_mut() {
                    obj.insert("value".into(), Value::String(value));
                }
            }
            None => arr.push(json!({ "key": name, "value": value })),
        }
    }
    let out = serde_json::to_string_pretty(&doc)?;
    fs::write(&file, out)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn req() -> LaunchReq {
        LaunchReq {
            overrides: vec![("dxgi".into(), "n,b".into())],
            env: vec![("PROTON_ENABLE_NVAPI".into(), "1".into())],
        }
    }

    #[test]
    fn enumerates_epic_and_gog() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("heroic");
        let epic_dir = t.path().join("games/Alpha");
        let gog_dir = t.path().join("games/Beta");
        fs::create_dir_all(&epic_dir).unwrap();
        fs::create_dir_all(&gog_dir).unwrap();
        write(
            &root.join("legendaryConfig/legendary/installed.json"),
            &format!(
                r#"{{"AlphaApp": {{"title": "Alpha Game", "install_path": "{}"}}}}"#,
                epic_dir.display()
            ),
        );
        write(
            &root.join("gog_store/installed.json"),
            &format!(
                r#"{{"installed": [{{"appName": "123", "install_path": "{}"}}]}}"#,
                gog_dir.display()
            ),
        );
        let got = games(&root);
        assert_eq!(got.len(), 2);
        let epic = got.iter().find(|g| g.store == "epic").unwrap();
        assert_eq!(epic.name, "Alpha Game");
        assert_eq!(epic.app_name, "AlphaApp");
        let gog = got.iter().find(|g| g.store == "gog").unwrap();
        assert_eq!(gog.app_name, "123");
        assert_eq!(gog.name, "123"); // falls back to appName without a title
    }

    #[test]
    fn apply_env_updates_each_key_spelling() {
        for key in ENV_KEYS {
            let t = tempfile::tempdir().unwrap();
            let root = t.path().to_path_buf();
            write(
                &root.join("GamesConfig/App.json"),
                &format!(
                    r#"{{"App": {{"{key}": [{{"key": "MANGOHUD", "value": "1"}}], "winePrefix": "/p"}}, "version": "v0"}}"#
                ),
            );
            apply_env(&root, "App", &req()).unwrap();
            let doc: Value = serde_json::from_str(
                &fs::read_to_string(root.join("GamesConfig/App.json")).unwrap(),
            )
            .unwrap();
            let arr = doc["App"][key].as_array().unwrap();
            assert_eq!(arr.len(), 3); // MANGOHUD kept + 2 of ours
            assert!(arr.iter().any(|e| e["key"] == "WINEDLLOVERRIDES"
                && e["value"] == "dxgi=n,b"));
            assert_eq!(doc["App"]["winePrefix"], "/p");
        }
    }

    #[test]
    fn apply_env_creates_array_when_absent_and_is_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        write(
            &root.join("GamesConfig/App.json"),
            r#"{"App": {"winePrefix": "/p"}, "version": "v0"}"#,
        );
        apply_env(&root, "App", &req()).unwrap();
        apply_env(&root, "App", &req()).unwrap();
        let doc: Value = serde_json::from_str(
            &fs::read_to_string(root.join("GamesConfig/App.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["App"]["enviromentOptions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn apply_env_missing_file_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        assert!(apply_env(t.path(), "Nope", &req()).is_err());
    }
}
