//! RTX Remix route. A Remix mod rebuilds an old DirectX 8/9 game with path
//! tracing; its runtime lives in a `.trex/` folder beside the game, and DLSS 5
//! neural rendering is a stage *inside* that runtime — nothing is injected, so
//! unlike every other route there is no ReShade, no feeder, no add-on. The
//! install is small: drop `nvngx_dlssnr.dll` into `.trex/` and flip one line in
//! `rtx.conf`. A Remix game present forces this route — a ReShade `dxgi.dll`
//! proxy lands on top of the Remix bridge and crashes it before it draws.
//!
//! This module is the pure, testable half: finding the runtime, reading which
//! neural fork it is, and the byte-safe `rtx.conf` splice. The download and
//! placement live in `installer.rs`, which has the network and the model.
//!
//! Experimental under Proton: Remix itself is a Vulkan path tracer that runs
//! under Proton, but whether the community DLSS-NR runtimes' NGX path fully
//! initialises there is the same dxvk-nvapi question the NR runtime hits
//! elsewhere — so the route ships honest about that.

use crate::game;
use std::fs;
use std::path::{Path, PathBuf};

/// The Remix runtime DLL inside `.trex/`. Its presence is what marks a real
/// runtime folder (an empty `.trex/` does not count).
pub const RUNTIME_DLL: &str = "d3d9.dll";

/// Directories never worth descending when hunting for `.trex/` — a Remix mod's
/// asset trees can hold hundreds of thousands of files.
const SKIP: [&str; 6] = ["mods", "rtx-remix", "gamedata", ".git", "reshade-shaders", "renodx"];

/// Which neural fork a Remix runtime is, by the config-key strings it embeds.
/// The two community forks name their option differently, so the runtime binary
/// is the authority — guessing wrong sets a key the runtime ignores.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    /// Kim2091's fork, inside xoxor4d's GTA IV mod: `rtx.neuralUplift.*`.
    Uplift,
    /// lunks' drop-in: `rtx.neuralRendering.*`.
    Neural,
    /// Both markers present.
    Both,
    /// No neural pass at all — NVIDIA's stock runtime. Needs the runtime swap.
    None,
}

impl Flavour {
    /// The `rtx.conf` key that turns the neural pass on for this fork.
    pub fn enable_key(self) -> Option<&'static str> {
        match self {
            Flavour::Uplift => Some("rtx.neuralUplift.enable"),
            Flavour::Neural | Flavour::Both => Some("rtx.neuralRendering.enable"),
            Flavour::None => None,
        }
    }

}

// ── catalogue of known RTX Remix projects ──────────────────────────
//
// So the tool can say "this game of yours has a Remix mod" and, for the few
// that publish a complete runtime as a plain zip, fetch and lay it in. Whether
// a project is actually installable is decided at download time (the release
// must carry a `.trex/` runtime), not asserted here — a mod can lose or gain a
// complete build between versions. `repo` names where to look; `link_only`
// projects are always just pointed to.

/// A game that has (or is) an RTX Remix build.
pub struct Project {
    /// The base game's name.
    pub game: &'static str,
    /// Who made the Remix mod (or "NVIDIA / Orbifold" for the official ones).
    pub by: &'static str,
    /// Where a person gets it — the mod's page.
    pub url: &'static str,
    /// `owner/repo` when the mod is on GitHub and *might* publish a complete
    /// zip this tool can install; `None` for a link-only project.
    pub repo: Option<&'static str>,
    /// Lower-case substrings that identify the base game by name or folder.
    pub names: &'static [&'static str],
    /// The game already ships as a Remix title — nothing to install, it *is*
    /// the game (Portal RTX and friends).
    pub official: bool,
}

/// Every RTX Remix project the tool knows about. A mod existing is not the same
/// as it running well — these are pointers, honestly labelled.
pub const PROJECTS: &[Project] = &[
    Project { game: "Portal with RTX", by: "NVIDIA", url: "https://store.steampowered.com/app/2012840", repo: None, names: &["portal with rtx"], official: true },
    Project { game: "Portal: Prelude RTX", by: "NVIDIA / Nicolas Grevet", url: "https://store.steampowered.com/app/2456740", repo: None, names: &["portal prelude rtx", "portal: prelude rtx"], official: true },
    Project { game: "Half-Life 2 RTX", by: "Orbifold Studios", url: "https://store.steampowered.com/app/2477540", repo: None, names: &["half-life 2 rtx", "half life 2 rtx"], official: true },
    Project { game: "Grand Theft Auto IV", by: "xoxor4d", url: "https://github.com/xoxor4d/gta4-rtx", repo: Some("xoxor4d/gta4-rtx"), names: &["grand theft auto iv", "gtaiv"], official: false },
    Project { game: "Need for Speed: Underground 2", by: "Ekozmaster", url: "https://github.com/Ekozmaster/NFSU2-RTX-Remix", repo: Some("Ekozmaster/NFSU2-RTX-Remix"), names: &["need for speed underground 2", "nfs underground 2", "underground 2"], official: false },
    Project { game: "Garry's Mod", by: "Xenthio", url: "https://github.com/Xenthio/garrys-mod-rtx-remixed", repo: None, names: &["garry's mod", "garrysmod", "gmod"], official: false },
    Project { game: "Deus Ex (2000)", by: "onnoj", url: "https://github.com/onnoj/DeusExEchelonRenderer", repo: None, names: &["deus ex: game of the year", "deus ex goty"], official: false },
    Project { game: "Thief Gold", by: "Night1099", url: "https://github.com/Night1099/thief-gold-rtx-remix", repo: None, names: &["thief gold"], official: false },
    Project { game: "The Elder Scrolls III: Morrowind", by: "BrunchyChineapple", url: "https://github.com/BrunchyChineapple/Morrowind-RTX-Remix-source", repo: None, names: &["morrowind"], official: false },
    Project { game: "Vampire: The Masquerade – Bloodlines", by: "CattoSalad", url: "https://github.com/CattoSalad/VTMB-RTX-Remix", repo: None, names: &["bloodlines", "vtmb"], official: false },
    Project { game: "Prince of Persia: The Sands of Time", by: "kaminoer", url: "https://github.com/kaminoer/pop-sot-rtx", repo: None, names: &["sands of time"], official: false },
    Project { game: "Saints Row 2", by: "BRAGme", url: "https://github.com/BRAGme/sr2-rtx-remix-proxy", repo: None, names: &["saints row 2"], official: false },
    Project { game: "Saints Row: The Third", by: "PurrsianMilkman", url: "https://github.com/PurrsianMilkman/Saints-Row-The-Third-RTX-REMIX-compatibility-mod", repo: None, names: &["saints row: the third", "saints row the third"], official: false },
    Project { game: "Red Faction", by: "BRAGme", url: "https://github.com/BRAGme/RedFaction-RTX", repo: None, names: &["red faction"], official: false },
    Project { game: "Total Overdose", by: "Utkar5hM", url: "https://github.com/Utkar5hM/TotalOverDoseRTXRemix", repo: None, names: &["total overdose"], official: false },
    Project { game: "Assassin's Creed II", by: "Kamzik123", url: "https://github.com/Kamzik123/ac2-rtx", repo: None, names: &["assassin's creed ii", "assassins creed ii"], official: false },
    Project { game: "Populous: The Beginning", by: "xmarre", url: "https://github.com/xmarre/Populous-3-RTX-Remix", repo: None, names: &["populous"], official: false },
    Project { game: "Silent Storm", by: "WormSlayer", url: "https://github.com/WormSlayer/silent-storm-rtx", repo: None, names: &["silent storm"], official: false },
    Project { game: "Dungeon Keeper 2", by: "mencelot", url: "https://github.com/mencelot/dk2-dxwrapper-with-path-tracing-support", repo: None, names: &["dungeon keeper 2"], official: false },
    Project { game: "Grand Theft Auto: Vice City", by: "GmanRO", url: "https://github.com/GmanRO/GTA-VICE-CITY-RTX-REMIX-.ASI-compiled-within-linux-", repo: None, names: &["vice city"], official: false },
    Project { game: "Cry of Fear", by: "michaelabilliot", url: "https://github.com/michaelabilliot/CryofFear_RTX-REMIX", repo: None, names: &["cry of fear"], official: false },
    Project { game: "Chess Titans", by: "Kamilkampfwagen-II", url: "https://github.com/Kamilkampfwagen-II/Chess-Titans-RTX", repo: None, names: &["chess titans"], official: false },
];

/// The directory prefix inside a mod's release zip that holds the whole Remix
/// runtime — the path up to (not including) the `.trex/` folder that contains
/// `d3d9.dll`. `""` when the runtime is at the zip root. `None` when the zip has
/// no complete runtime at all (a source tree or a bare proxy), which is how a
/// link-only release is told apart from an installable one.
pub fn mod_root(members: &[String]) -> Option<String> {
    const NEEDLE: &str = ".trex/d3d9.dll";
    members.iter().find_map(|m| {
        let norm = m.replace('\\', "/");
        norm.to_ascii_lowercase()
            .ends_with(NEEDLE)
            .then(|| norm[..norm.len() - NEEDLE.len()].to_string())
    })
}

/// Strip `root` from the front of a normalised zip path, case-insensitively
/// (some archives mix case between the descriptor and the entries). `None` when
/// the path is not under `root`.
pub fn strip_root<'a>(member_norm: &'a str, root: &str) -> Option<&'a str> {
    (member_norm.len() >= root.len()
        && member_norm[..root.len()].eq_ignore_ascii_case(root))
    .then(|| &member_norm[root.len()..])
}

/// The Remix project a game name/folder belongs to, if any. `text` is matched
/// lower-cased against each project's `names`, longest name first so a specific
/// entry wins over a looser one.
pub fn match_project(text: &str) -> Option<&'static Project> {
    let t = text.to_ascii_lowercase();
    let mut best: Option<(usize, &'static Project)> = None;
    for p in PROJECTS {
        for n in p.names {
            if t.contains(n) && best.is_none_or(|(len, _)| n.len() > len) {
                best = Some((n.len(), p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The `.trex/` runtime folder beside or just below the game exe's folder, or
/// `None` when this is not a Remix game. Breadth-capped so a mod's asset tree
/// is not walked.
pub fn find_runtime(root: &Path) -> Option<PathBuf> {
    fn walk(d: &Path, depth: u8) -> Option<PathBuf> {
        let rd = fs::read_dir(d).ok()?;
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.eq_ignore_ascii_case(".trex") && game::join_ci(&p, &[RUNTIME_DLL]).is_file() {
                return Some(p);
            }
            let lower = name.to_ascii_lowercase();
            if !SKIP.iter().any(|s| lower == *s) {
                subdirs.push(p);
            }
        }
        if depth > 0 {
            for sub in subdirs {
                if let Some(hit) = walk(&sub, depth - 1) {
                    return Some(hit);
                }
            }
        }
        None
    }
    walk(root, 3)
}

/// Which fork the runtime in this `.trex/` is. Reads the (large) runtime DLL and
/// scans for the two config-key markers.
pub fn flavour(trex: &Path) -> Flavour {
    match fs::read(game::join_ci(trex, &[RUNTIME_DLL])) {
        Ok(bytes) => flavour_in(&bytes),
        Err(_) => Flavour::None,
    }
}

fn flavour_in(bytes: &[u8]) -> Flavour {
    let has = |needle: &[u8]| memchr::memmem::find(bytes, needle).is_some();
    match (has(b"rtx.neuralUplift"), has(b"rtx.neuralRendering")) {
        (true, true) => Flavour::Both,
        (true, false) => Flavour::Uplift,
        (false, true) => Flavour::Neural,
        (false, false) => Flavour::None,
    }
}

/// The `rtx.conf` Remix reads for this runtime: the mod author's own, next to
/// the `.trex/` folder, preferred over one we would create.
pub fn conf_path(trex: &Path) -> PathBuf {
    let parent = trex.parent().unwrap_or(trex);
    let existing = game::join_ci(parent, &["rtx.conf"]);
    if existing.is_file() {
        existing
    } else {
        parent.join("rtx.conf")
    }
}

/// True when either fork's neural pass is already enabled in this `rtx.conf`.
pub fn any_enabled(conf_text: &str) -> bool {
    ["rtx.neuralUplift.enable", "rtx.neuralRendering.enable"]
        .iter()
        .any(|k| option_value(conf_text, k).is_some_and(|v| v.eq_ignore_ascii_case("true")))
}

/// The value of `key` in an `rtx.conf`, if the line is present.
pub fn option_value(conf_text: &str, key: &str) -> Option<String> {
    conf_text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// Set `key = value` in an `rtx.conf`, byte-safe: replace the line in place
/// (keeping its own line ending) or append it, reusing the file's newline style
/// and fixing a missing final newline first, so a hand-edited conf is never
/// corrupted.
pub fn set_option(conf_text: &str, key: &str, value: &str) -> String {
    let nl = if conf_text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(conf_text.len() + key.len() + value.len() + 4);
    let mut found = false;
    for line in conf_text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        if !found {
            if let Some((k, _)) = content.split_once('=') {
                if k.trim() == key {
                    out.push_str(&format!("{key} = {value}{ending}"));
                    found = true;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    if !found {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&format!("{key} = {value}{nl}"));
    }
    out
}

/// Remove `key`'s line from an `rtx.conf` (used by uninstall).
pub fn remove_option(conf_text: &str, key: &str) -> String {
    conf_text
        .split_inclusive('\n')
        .filter(|line| {
            let content = line.trim_end_matches(['\r', '\n']);
            !content
                .split_once('=')
                .is_some_and(|(k, _)| k.trim() == key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavour_reads_the_fork_markers() {
        assert_eq!(flavour_in(b"...rtx.neuralUplift.enable..."), Flavour::Uplift);
        assert_eq!(
            flavour_in(b"junk rtx.neuralRendering.enable junk"),
            Flavour::Neural
        );
        assert_eq!(
            flavour_in(b"rtx.neuralUplift rtx.neuralRendering"),
            Flavour::Both
        );
        assert_eq!(flavour_in(b"a stock runtime with neither"), Flavour::None);
        assert_eq!(Flavour::Uplift.enable_key(), Some("rtx.neuralUplift.enable"));
        assert_eq!(
            Flavour::Neural.enable_key(),
            Some("rtx.neuralRendering.enable")
        );
        assert_eq!(Flavour::None.enable_key(), None);
    }

    #[test]
    fn mod_root_finds_the_runtime_subtree_or_says_no() {
        // Runtime one folder down: the prefix strips that folder.
        let m = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let members = m(&[
            "GTAIV-Remix/",
            "GTAIV-Remix/.trex/d3d9.dll",
            "GTAIV-Remix/.trex/foo.dll",
            "GTAIV-Remix/rtx.conf",
        ]);
        let root = mod_root(&members).unwrap();
        assert_eq!(root, "GTAIV-Remix/");
        assert_eq!(strip_root("GTAIV-Remix/.trex/foo.dll", &root), Some(".trex/foo.dll"));
        assert_eq!(strip_root("GtaIV-Remix/rtx.conf", &root), Some("rtx.conf")); // case-insensitive
        assert_eq!(strip_root("Other/x", &root), None);

        // Runtime at the root: empty prefix.
        assert_eq!(mod_root(&m(&[".trex/d3d9.dll", "rtx.conf"])), Some(String::new()));
        // Backslash archives normalise.
        assert_eq!(mod_root(&m(&["Mod\\.trex\\d3d9.dll"])), Some("Mod/".to_string()));
        // A source tree / bare proxy is not installable.
        assert_eq!(mod_root(&m(&["src/main.cpp", "d3d9.dll"])), None);
    }

    #[test]
    fn match_project_recognises_games_by_name_or_folder() {
        assert_eq!(
            match_project("The Elder Scrolls III: Morrowind").map(|p| p.game),
            Some("The Elder Scrolls III: Morrowind")
        );
        // Folder-name form works too, and matching is case-insensitive.
        assert_eq!(match_project("GTAIV").map(|p| p.repo), Some(Some("xoxor4d/gta4-rtx")));
        // The specific "underground 2" wins; an unrelated game matches nothing.
        assert!(match_project("Need for Speed Underground 2").is_some());
        assert!(match_project("Cyberpunk 2077").is_none());
        // The official Remix titles are flagged as already-Remix.
        assert!(match_project("Portal with RTX").unwrap().official);
        // Every catalogue entry has at least one match string and a real URL.
        for p in PROJECTS {
            assert!(!p.names.is_empty() && p.url.starts_with("http"), "{}", p.game);
        }
    }

    #[test]
    fn find_runtime_needs_a_trex_with_the_runtime_dll() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        assert_eq!(find_runtime(root), None);
        // An empty .trex does not count.
        fs::create_dir_all(root.join("bin").join(".trex")).unwrap();
        assert_eq!(find_runtime(root), None);
        // With the runtime dll, it is found (one level down, Portal-style).
        fs::write(root.join("bin").join(".trex").join("d3d9.dll"), b"x").unwrap();
        assert_eq!(find_runtime(root), Some(root.join("bin").join(".trex")));
    }

    #[test]
    fn set_option_replaces_in_place_or_appends() {
        // Append when absent, using the file's newline and fixing a missing one.
        let out = set_option("rtx.foo = 1", "rtx.neuralUplift.enable", "True");
        assert_eq!(out, "rtx.foo = 1\nrtx.neuralUplift.enable = True\n");
        // Replace in place, keeping other lines and the CRLF style.
        let conf = "rtx.a = 1\r\nrtx.neuralUplift.enable = False\r\nrtx.b = 2\r\n";
        let out = set_option(conf, "rtx.neuralUplift.enable", "True");
        assert_eq!(
            out,
            "rtx.a = 1\r\nrtx.neuralUplift.enable = True\r\nrtx.b = 2\r\n"
        );
        assert!(any_enabled(&out));
        // Idempotent value, and remove takes it back out.
        assert_eq!(set_option(&out, "rtx.neuralUplift.enable", "True"), out);
        let removed = remove_option(&out, "rtx.neuralUplift.enable");
        assert_eq!(removed, "rtx.a = 1\r\nrtx.b = 2\r\n");
        assert!(!any_enabled(&removed));
    }
}
