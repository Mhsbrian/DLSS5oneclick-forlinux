//! Lutris: enumerate installed Wine games from `~/.local/share/lutris/games/*.yml`.
//!
//! List-only in v1 — the YAMLs are machine-written but free-form enough
//! (installer `script:` blocks nest a second `game:` at deeper indent) that
//! surgical writes are not worth the risk; after an install the UI shows the
//! env vars to add under the game's Lutris settings instead.
//!
//! The reader is a deliberate non-YAML: it only takes `name:`/`slug:`/
//! `game_slug:` at zero indent and `exe:`/`prefix:`/`working_dir:` at exactly
//! two-space indent under a zero-indent `game:` line — verified against real
//! Lutris output; anything else is ignored.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LutrisGame {
    pub name: String,
    pub slug: String,
    /// Absolute game exe (a prefix-relative `exe:` joined onto `prefix:`).
    pub exe: Option<PathBuf>,
    /// The folder holding the exe, when known.
    pub dir: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
pub fn data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = Path::new(&home).join(".local/share/lutris");
    p.is_dir().then_some(p)
}

pub fn games_from(data_dir: &Path) -> Vec<LutrisGame> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(data_dir.join("games")) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("yml") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        if let Some(g) = parse_game_yml(&text, &p) {
            out.push(g);
        }
    }
    out.sort_by_key(|g| g.name.to_lowercase());
    out
}

fn parse_game_yml(text: &str, path: &Path) -> Option<LutrisGame> {
    let mut name = None;
    let mut slug = None;
    let mut exe = None;
    let mut prefix = None;
    let mut working_dir = None;
    let mut in_game_block = false;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim_end();
        if indent == 0 {
            in_game_block = t == "game:";
            if let Some(v) = t.strip_prefix("name: ") {
                name = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("slug: ") {
                slug = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("game_slug: ") {
                slug.get_or_insert_with(|| v.trim().to_string());
            }
            continue;
        }
        if in_game_block && indent == 2 {
            let t = t.trim_start();
            if let Some(v) = t.strip_prefix("exe: ") {
                exe = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("prefix: ") {
                prefix = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("working_dir: ") {
                working_dir = Some(v.trim().to_string());
            }
        }
    }
    let slug = slug.or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
    })?;
    let name = name.unwrap_or_else(|| slug.clone());
    let exe = exe.map(|e| {
        let e = PathBuf::from(e);
        if e.is_absolute() {
            e
        } else if let Some(base) = prefix.as_deref().or(working_dir.as_deref()) {
            Path::new(base).join(e)
        } else {
            e
        }
    });
    let dir = exe
        .as_ref()
        .and_then(|e| e.parent())
        .map(Path::to_path_buf);
    Some(LutrisGame {
        name,
        slug,
        exe,
        dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_shape_with_nested_script_game() {
        // Shape taken from a real machine-written Lutris file: the nested
        // `script: game:` block must not override the top-level one.
        let text = "game:\n  arch: win64\n  exe: drive_c/Program Files (x86)/Battle.net/Battle.net Launcher.exe\n  prefix: /home/u/Games/battlenet\ngame_slug: battlenet\nname: Battle.net\nscript:\n  game:\n    arch: win64\n    exe: drive_c/EVIL/other.exe\n    prefix: $GAMEDIR\n  system:\n    env:\n      DXVK_HUD: compiler\nslug: battlenet-standard\nversion: Standard\n";
        let g = parse_game_yml(text, Path::new("/x/games/battlenet-123.yml")).unwrap();
        assert_eq!(g.name, "Battle.net");
        assert_eq!(g.slug, "battlenet-standard");
        assert_eq!(
            g.exe.as_deref(),
            Some(Path::new(
                "/home/u/Games/battlenet/drive_c/Program Files (x86)/Battle.net/Battle.net Launcher.exe"
            ))
        );
        assert!(g.dir.unwrap().ends_with("Battle.net"));
    }

    #[test]
    fn flat_file_and_filename_slug_fallback() {
        let text = "game:\n  exe: /abs/path/Game/game.exe\n";
        let g = parse_game_yml(text, Path::new("/x/games/foo-42.yml")).unwrap();
        assert_eq!(g.slug, "foo-42");
        assert_eq!(g.name, "foo-42");
        assert_eq!(g.exe.as_deref(), Some(Path::new("/abs/path/Game/game.exe")));
    }

    #[test]
    fn broken_files_are_skipped() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        fs::create_dir_all(d.join("games")).unwrap();
        fs::write(d.join("games/ok-1.yml"), "game:\n  exe: /g/a.exe\nname: Ok\n").unwrap();
        fs::write(d.join("games/none.txt"), "not yaml").unwrap();
        let games = games_from(d);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "Ok");
    }
}
