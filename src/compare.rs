//! Before / after: pair the two newest ReShade screenshots so the neural
//! effect can be seen side by side. The workflow is the community's own —
//! shoot with neural rendering off, toggle it on, shoot again — and this finds
//! that pair without the user hunting through a folder that can hold thousands
//! of files.
//!
//! Read-only: it never writes, and it looks only at the top level of the game
//! folder and ReShade's screenshot folder (a game tree can be enormous, and a
//! deep walk is not worth it for two files ReShade drops right there).

use crate::game;
use crate::reshade_ini::Ini;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Image kinds ReShade writes that the bundled `image` decoder can show.
const EXTS: [&str; 3] = ["png", "jpg", "jpeg"];

/// Two shots taken more than this apart are not one before/after pair — they
/// are two unrelated sessions, and pairing them would be misleading.
const PAIR_WINDOW: Duration = Duration::from_secs(300);

/// ReShade's screenshot folder for this game: `[SCREENSHOT] SavePath` in
/// `ReShade.ini`, resolved against the game folder (a relative path is joined,
/// an absolute one taken as is). The game folder itself when unset — ReShade's
/// own default.
pub fn save_path(game_dir: &Path) -> PathBuf {
    let ini = game::join_ci(game_dir, &["ReShade.ini"]);
    if let Ok(text) = std::fs::read_to_string(&ini) {
        if let Some(sp) = Ini::parse(&text).get("SCREENSHOT", "SavePath") {
            let sp = sp.trim().replace('\\', "/");
            if !sp.is_empty() {
                let p = Path::new(&sp);
                return if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    game_dir.join(p)
                };
            }
        }
    }
    game_dir.to_path_buf()
}

/// Every image file directly in `dir`, with its modified time.
fn shots_in(dir: &Path) -> Vec<(SystemTime, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let is_img = p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| EXTS.iter().any(|e| e.eq_ignore_ascii_case(x)));
        if !is_img {
            continue;
        }
        if let Ok(mt) = e.metadata().and_then(|m| m.modified()) {
            if p.is_file() {
                out.push((mt, p));
            }
        }
    }
    out
}

/// The two newest of a set as (before, after) — oldest of the two first — but
/// only when they were taken within `PAIR_WINDOW` of each other. Pure, so the
/// selection is tested without depending on real file timestamps.
fn pick_pair(mut shots: Vec<(SystemTime, PathBuf)>) -> Option<(PathBuf, PathBuf)> {
    shots.sort_by_key(|a| std::cmp::Reverse(a.0)); // newest first
    let (t_after, after) = shots.first()?.clone();
    let (t_before, before) = shots.get(1)?.clone();
    if t_after.duration_since(t_before).unwrap_or_default() > PAIR_WINDOW {
        return None;
    }
    Some((before, after))
}

/// The newest before/after pair for this game, across the game folder and
/// ReShade's screenshot folder. `None` until two shots exist close together.
pub fn newest_pair(game_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    use std::collections::HashMap;
    let mut by_path: HashMap<PathBuf, SystemTime> = HashMap::new();
    for (t, p) in shots_in(game_dir) {
        by_path.insert(p, t);
    }
    let sp = save_path(game_dir);
    if sp != game_dir {
        for (t, p) in shots_in(&sp) {
            by_path.entry(p).or_insert(t);
        }
    }
    pick_pair(by_path.into_iter().map(|(p, t)| (t, p)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn pick_pair_takes_the_two_newest_oldest_first() {
        let shots = vec![
            (at(1000), PathBuf::from("a.png")),
            (at(1010), PathBuf::from("b.png")), // after (newest)
            (at(1005), PathBuf::from("c.png")), // before (2nd newest)
        ];
        assert_eq!(
            pick_pair(shots),
            Some((PathBuf::from("c.png"), PathBuf::from("b.png")))
        );
    }

    #[test]
    fn pick_pair_needs_two_within_the_window() {
        assert_eq!(pick_pair(vec![(at(1000), PathBuf::from("only.png"))]), None);
        // 6 minutes apart: two different sessions, not a pair.
        let far = vec![
            (at(1000), PathBuf::from("old.png")),
            (at(1360), PathBuf::from("new.png")),
        ];
        assert_eq!(pick_pair(far), None);
    }

    #[test]
    fn save_path_reads_reshade_ini_or_defaults_to_the_game_folder() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        // No ini: the game folder itself.
        assert_eq!(save_path(d), d.to_path_buf());
        // Relative SavePath (ReShade writes Windows separators) is joined.
        std::fs::write(
            d.join("ReShade.ini"),
            "[SCREENSHOT]\nSavePath=.\\screens\nSoundPath=x\n",
        )
        .unwrap();
        assert_eq!(save_path(d), d.join("screens"));
    }

    #[test]
    fn newest_pair_finds_two_shots_on_disk() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert_eq!(newest_pair(d), None);
        std::fs::write(d.join("before.png"), b"x").unwrap();
        // Ensure a distinct, later mtime for the second shot.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(d.join("after.png"), b"y").unwrap();
        let (before, after) = newest_pair(d).expect("a pair");
        assert_eq!(before.file_name().unwrap(), "before.png");
        assert_eq!(after.file_name().unwrap(), "after.png");
        // A non-image file never counts.
        std::fs::write(d.join("notes.txt"), b"z").unwrap();
        assert_eq!(newest_pair(d).map(|(_, a)| a), Some(d.join("after.png")));
    }
}
