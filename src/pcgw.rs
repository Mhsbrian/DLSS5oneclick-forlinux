//! PCGamingWiki as a last resort for a game's graphics API.
//!
//! When the exe's imports and the game's own log (see `game::detect_api`) both
//! leave the API unknown, the wiki's per-game "API" block ("direct3d versions
//! = 12") can settle it. Two plain requests per game, no key: the Steam app ID
//! redirects to the game's page (`/api/appid.php`), and the page's wikitext
//! (`action=parse`) carries the block. The wiki's query API is closed to
//! arbitrary Cargo queries and its export sits behind a browser check, so this
//! is the route that works (checked 2026-10-02).
//!
//! Detection itself only ever reads the cache (`cached_api`), so a window never
//! waits on the network; `warm` fills it from a background thread or the
//! command line. Only a game with a Steam app ID is looked up, one lookup per
//! game, and each answer is kept (a found API for 60 days, "the wiki has no
//! single answer" for 7, a failed request for an hour). `DLSS5ONECLICK_NO_PCGW=1`
//! turns it off. The wiki's text is CC BY-NC-SA 3.0; only the API facts are read.

use crate::game::Api;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: u64 = 24 * 3600;
const FOUND_TTL: u64 = 60 * DAY;
const NONE_TTL: u64 = 7 * DAY;
const ERROR_TTL: u64 = 3600;

pub fn disabled() -> bool {
    std::env::var_os("DLSS5ONECLICK_NO_PCGW").is_some()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_file() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dlss5oneclick")
        .join("pcgw-cache.json")
}

fn label(a: Option<Api>) -> &'static str {
    match a {
        Some(Api::Dx12) => "dx12",
        Some(Api::Dx11) => "dx11",
        Some(Api::Dx10) => "dx10",
        Some(Api::Dx9) => "dx9",
        Some(Api::Vulkan) => "vulkan",
        _ => "none",
    }
}

fn from_label(s: &str) -> Option<Api> {
    match s {
        "dx12" => Some(Api::Dx12),
        "dx11" => Some(Api::Dx11),
        "dx10" => Some(Api::Dx10),
        "dx9" => Some(Api::Dx9),
        "vulkan" => Some(Api::Vulkan),
        _ => None,
    }
}

/// What a cache entry says, when it is still fresh: `Some(Some(api))` a found
/// API, `Some(None)` nothing usable, `None` not looked up (or stale).
fn read_entry(cache: &Value, appid: u64, at: u64) -> Option<Option<Api>> {
    let e = cache.get(appid.to_string())?;
    let api = e.get("api")?.as_str()?;
    let t = e.get("t")?.as_u64()?;
    let ttl = match api {
        "err" => ERROR_TTL,
        "none" => NONE_TTL,
        _ => FOUND_TTL,
    };
    if at.saturating_sub(t) > ttl {
        return None;
    }
    Some(from_label(api))
}

fn load(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn store(path: &Path, appid: u64, api: &str) {
    let mut c = load(path);
    c[appid.to_string()] = json!({ "api": api, "t": now() });
    if let Some(d) = path.parent() {
        let _ = fs::create_dir_all(d);
    }
    let _ = fs::write(path, c.to_string());
}

/// The first whole number of each comma-separated value ("11, 12" -> 11 and 12,
/// "9.0c" -> 9, "10.1" -> 10); nothing for an empty or text value.
fn versions(v: &str) -> Vec<u32> {
    v.split(',')
        .filter_map(|t| {
            let d: String = t
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            d.parse().ok()
        })
        .collect()
}

fn field(wikitext: &str, key: &str) -> String {
    for l in wikitext.lines() {
        let l = l.trim_start();
        let Some(rest) = l.strip_prefix('|') else {
            continue;
        };
        let Some((k, v)) = rest.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(key) {
            return v.trim().to_owned();
        }
    }
    String::new()
}

/// The single API a page's `{{API}}` block settles. A game listing both
/// Direct3D 11 and 12 (the player picks) settles nothing; a Vulkan-only game
/// does, and a game with both Direct3D and Vulkan goes by its Direct3D line.
pub fn api_from_wikitext(w: &str) -> Option<Api> {
    let d3d = versions(&field(w, "direct3d versions"));
    let vk = !versions(&field(w, "vulkan versions")).is_empty();
    let top = |v: &[u32]| v.iter().copied().max();
    match (d3d.as_slice(), vk) {
        ([], true) => Some(Api::Vulkan),
        ([], false) => None,
        (d, _) => {
            let has = |n: u32| d.contains(&n);
            if has(11) && has(12) {
                None
            } else {
                match top(d)? {
                    12 => Some(Api::Dx12),
                    11 => Some(Api::Dx11),
                    10 => Some(Api::Dx10),
                    9 => Some(Api::Dx9),
                    _ => None,
                }
            }
        }
    }
}

/// The Steam app ID of an install under `steamapps\common\<folder>\...`, from
/// the `appmanifest_*.acf` whose `installdir` is that folder.
pub fn steam_appid(exe: &Path) -> Option<u64> {
    let comps: Vec<_> = exe.ancestors().collect();
    let common = comps.iter().find(|a| {
        a.file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("common"))
            && a.parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n.eq_ignore_ascii_case("steamapps"))
    })?;
    let steamapps = common.parent()?;
    let folder = exe
        .strip_prefix(common)
        .ok()?
        .components()
        .next()?
        .as_os_str()
        .to_string_lossy()
        .to_ascii_lowercase();
    for e in fs::read_dir(steamapps).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(id) = name
            .strip_prefix("appmanifest_")
            .and_then(|r| r.strip_suffix(".acf"))
            .and_then(|r| r.parse::<u64>().ok())
        else {
            continue;
        };
        let Ok(text) = fs::read_to_string(e.path()) else {
            continue;
        };
        let dir = text.lines().find_map(|l| {
            let l = l.trim();
            let r = l.strip_prefix("\"installdir\"")?;
            Some(r.trim().trim_matches('"').to_ascii_lowercase())
        });
        if dir.as_deref() == Some(folder.as_str()) {
            return Some(id);
        }
    }
    None
}

/// The cached answer for this game, never touching the network.
pub fn cached_api(exe: &Path) -> Option<Api> {
    if disabled() {
        return None;
    }
    let appid = steam_appid(exe)?;
    read_entry(&load(&cache_file()), appid, now())?
}

fn fetch(appid: u64) -> Result<Option<Api>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!(
            "DLSS5oneclick/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/faisalkindi/DLSS5oneclick)"
        ))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .context("cannot build HTTP client")?;
    let r = client
        .get(format!(
            "https://www.pcgamingwiki.com/api/appid.php?appid={appid}"
        ))
        .send()
        .context("PCGamingWiki is not reachable")?;
    let Some(loc) = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
    else {
        // No page for this app ID.
        return Ok(None);
    };
    let page = loc.rsplit("/wiki/").next().unwrap_or("").to_owned();
    if page.is_empty() {
        return Ok(None);
    }
    // The redirect carries the page title already percent-encoded.
    let body: Value = client
        .get(format!(
            "https://www.pcgamingwiki.com/w/api.php?action=parse&format=json&prop=wikitext&page={page}"
        ))
        .send()
        .context("PCGamingWiki page request failed")?
        .error_for_status()?
        .json()
        .context("PCGamingWiki answered with something that is not JSON")?;
    let text = body["parse"]["wikitext"]["*"]
        .as_str()
        .context("PCGamingWiki page has no wikitext")?;
    Ok(api_from_wikitext(text))
}

/// Look this game up once if the cache has no fresh answer. For a background
/// thread or the command line: it waits on the network.
pub fn warm(exe: &Path) {
    if disabled() {
        return;
    }
    let Some(appid) = steam_appid(exe) else {
        return;
    };
    let path = cache_file();
    if read_entry(&load(&path), appid, now()).is_some() {
        return;
    }
    match fetch(appid) {
        Ok(api) => store(&path, appid, label(api)),
        Err(_) => store(&path, appid, "err"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KCD2: &str = "{{Infobox game\n|steam appid  = 1771300\n}}\n{{API\n|direct3d versions      = 12\n|opengl versions        = \n|vulkan versions        = \n|windows 32-bit exe     = false\n}}";

    #[test]
    fn the_api_block_settles_one_api() {
        assert_eq!(api_from_wikitext(KCD2), Some(Api::Dx12));
        let w = |d: &str, v: &str| {
            format!("{{{{API\n|direct3d versions = {d}\n|vulkan versions = {v}\n}}}}")
        };
        assert_eq!(api_from_wikitext(&w("11", "")), Some(Api::Dx11));
        assert_eq!(api_from_wikitext(&w("9.0c", "")), Some(Api::Dx9));
        assert_eq!(api_from_wikitext(&w("10.1", "")), Some(Api::Dx10));
        // A game with both Direct3D lines lets the player choose: no answer.
        assert_eq!(api_from_wikitext(&w("11, 12", "")), None);
        // Vulkan alone settles it; Direct3D beside Vulkan goes by Direct3D.
        assert_eq!(api_from_wikitext(&w("", "1.3")), Some(Api::Vulkan));
        assert_eq!(api_from_wikitext(&w("12", "1.3")), Some(Api::Dx12));
        assert_eq!(api_from_wikitext(&w("", "")), None);
        assert_eq!(api_from_wikitext("no block at all"), None);
    }

    #[test]
    fn a_cache_entry_expires_by_kind() {
        let c = json!({
            "1": {"api": "dx12", "t": 1000},
            "2": {"api": "none", "t": 1000},
            "3": {"api": "err", "t": 1000},
        });
        assert_eq!(read_entry(&c, 1, 1000 + 59 * DAY), Some(Some(Api::Dx12)));
        assert_eq!(read_entry(&c, 1, 1000 + 61 * DAY), None);
        assert_eq!(read_entry(&c, 2, 1000 + 6 * DAY), Some(None));
        assert_eq!(read_entry(&c, 2, 1000 + 8 * DAY), None);
        assert_eq!(read_entry(&c, 3, 1000 + 1800), Some(None));
        assert_eq!(read_entry(&c, 3, 1000 + 7200), None);
        assert_eq!(read_entry(&c, 9, 1000), None);
    }

    #[test]
    fn the_cache_roundtrips_and_survives_garbage() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sub").join("c.json");
        assert!(load(&p).as_object().unwrap().is_empty());
        store(&p, 7, "dx11");
        store(&p, 8, "none");
        let c = load(&p);
        assert_eq!(read_entry(&c, 7, now()), Some(Some(Api::Dx11)));
        assert_eq!(read_entry(&c, 8, now()), Some(None));
        fs::write(&p, "not json").unwrap();
        assert!(load(&p).as_object().unwrap().is_empty());
    }

    #[test]
    fn a_steam_install_is_told_from_its_manifest() {
        let t = tempfile::tempdir().unwrap();
        let sa = t.path().join("steamapps");
        let game = sa.join("common").join("Some Game").join("bin");
        fs::create_dir_all(&game).unwrap();
        fs::write(
            sa.join("appmanifest_4242.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"4242\"\n\t\"installdir\"\t\t\"Some Game\"\n}\n",
        )
        .unwrap();
        fs::write(
            sa.join("appmanifest_1.acf"),
            "\"AppState\"\n{\n\t\"installdir\"\t\t\"Other\"\n}\n",
        )
        .unwrap();
        assert_eq!(steam_appid(&game.join("game.exe")), Some(4242));
        // Not under steamapps\\common: not a Steam install.
        assert_eq!(steam_appid(&t.path().join("x").join("game.exe")), None);
    }
}
