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
const SKIP: [&str; 6] = ["mods", "rtx-remix", "g[a-z]", ".git", "reshade-shaders", "renodx"];

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
