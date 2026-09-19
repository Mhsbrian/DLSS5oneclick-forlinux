//! Non-Steam games ("Add a Non-Steam Game to My Library") live in
//! `userdata/<uid>/config/shortcuts.vdf`, not in appmanifests. Each entry's
//! `appid` is the id Steam names the game's Proton prefix after
//! (`steamapps/compatdata/<appid>` in the Steam root's own library) and keys its
//! CompatToolMapping with, so once listed a shortcut behaves like any Steam
//! game for Proton lookups. Its launch options sit in the entry itself.
//!
//! The file is Valve's *binary* KeyValues, not the text format `vdf.rs` reads:
//! a type byte (0x00 object, 0x01 string, 0x02 u32, 0x08 end), a NUL-terminated
//! key, then the value (NUL-terminated string, little-endian u32, or nested
//! fields up to 0x08). Nothing carries a length or an offset, so replacing one
//! string's bytes leaves every other byte valid: the edit splices exactly that
//! value and verifies the rest re-parses identical, mirroring `steam::apply_with`.

use super::launch_options::{self, LaunchReq};
use super::steam::ApplyOutcome;
use anyhow::{bail, Context, Result};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

const T_OBJECT: u8 = 0x00;
const T_STRING: u8 = 0x01;
const T_INT32: u8 = 0x02;
const T_END: u8 = 0x08;

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Obj(Vec<Field>),
    /// Raw bytes and where they sit in the file (NUL excluded).
    Str(Vec<u8>, Range<usize>),
    Int(u32),
}

#[derive(Debug, Clone, PartialEq)]
struct Field {
    key: String,
    val: Val,
}

fn cstr(d: &[u8], pos: &mut usize) -> Result<Range<usize>> {
    let start = *pos;
    let Some(len) = d.get(start..).and_then(|rest| rest.iter().position(|&b| b == 0)) else {
        bail!("unterminated string at byte {start}");
    };
    *pos = start + len + 1;
    Ok(start..start + len)
}

fn fields(d: &[u8], pos: &mut usize, depth: usize) -> Result<Vec<Field>> {
    if depth > 16 {
        bail!("nesting too deep at byte {}", *pos);
    }
    let mut out = Vec::new();
    loop {
        let Some(&t) = d.get(*pos) else {
            bail!("truncated: missing end marker");
        };
        *pos += 1;
        if t == T_END {
            return Ok(out);
        }
        let k = cstr(d, pos)?;
        let key = String::from_utf8_lossy(&d[k]).into_owned();
        let val = match t {
            T_OBJECT => Val::Obj(fields(d, pos, depth + 1)?),
            T_STRING => {
                let r = cstr(d, pos)?;
                Val::Str(d[r.clone()].to_vec(), r)
            }
            T_INT32 => {
                let Some(b) = d.get(*pos..*pos + 4) else {
                    bail!("truncated integer at byte {}", *pos);
                };
                *pos += 4;
                Val::Int(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            }
            other => bail!("unknown field type 0x{other:02x} ({key:?})"),
        };
        out.push(Field { key, val });
    }
}

/// The file's top-level fields.
fn parse(d: &[u8]) -> Result<Vec<Field>> {
    fields(d, &mut 0, 0)
}

fn get<'a>(f: &'a [Field], key: &str) -> Option<&'a Val> {
    f.iter()
        .find(|x| x.key.eq_ignore_ascii_case(key))
        .map(|x| &x.val)
}

fn text(f: &[Field], key: &str) -> Option<String> {
    match get(f, key)? {
        Val::Str(b, _) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

fn entries(root: &[Field]) -> impl Iterator<Item = &[Field]> + '_ {
    let list: &[Field] = match get(root, "shortcuts") {
        Some(Val::Obj(v)) => v,
        _ => &[],
    };
    list.iter().filter_map(|e| match &e.val {
        Val::Obj(f) => Some(f.as_slice()),
        _ => None,
    })
}

/// Unsigned decimal, the way the compatdata folder spells it.
fn appid_of(e: &[Field]) -> Option<String> {
    match get(e, "appid")? {
        Val::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub appid: String,
    pub name: String,
    /// `Exe` as Steam stores it, usually wrapped in double quotes.
    pub exe: String,
    pub launch_options: String,
}

fn unquote(s: &str) -> &str {
    let t = s.trim();
    t.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(t)
}

impl Shortcut {
    /// The exe's folder. Not `StartDir`: that can be any folder at all (even
    /// the home folder), and a broad one would claim every game folder beneath
    /// it in `entry_for_path`.
    pub fn dir(&self) -> Option<PathBuf> {
        Path::new(unquote(&self.exe))
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
    }

    /// A Windows program: the only kind Proton, and so this tool, can set up.
    pub fn is_windows_exe(&self) -> bool {
        unquote(&self.exe).to_ascii_lowercase().ends_with(".exe")
    }
}

/// Every shortcut in one file. Entries without an appid (very old Steam) are
/// skipped: without one there is no prefix to find.
pub fn parse_shortcuts(d: &[u8]) -> Result<Vec<Shortcut>> {
    let root = parse(d)?;
    Ok(entries(&root)
        .filter_map(|e| {
            Some(Shortcut {
                appid: appid_of(e)?,
                name: text(e, "AppName")?,
                exe: text(e, "Exe").unwrap_or_default(),
                launch_options: text(e, "LaunchOptions").unwrap_or_default(),
            })
        })
        .collect())
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(root.join("userdata"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("config/shortcuts.vdf"))
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutGame {
    pub appid: String,
    pub name: String,
    pub dir: PathBuf,
}

/// Windows-program shortcuts across every Steam user of `root` whose folder
/// exists, one per appid.
pub fn games(root: &Path) -> Vec<ShortcutGame> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out: Vec<ShortcutGame> = Vec::new();
    for file in files(root) {
        let Some(list) = fs::read(&file).ok().and_then(|d| parse_shortcuts(&d).ok()) else {
            continue;
        };
        for s in list {
            let Some(dir) = s.dir() else { continue };
            if !s.is_windows_exe()
                || !dir.is_dir()
                || home.as_deref() == Some(dir.as_path())
                || out.iter().any(|g| g.appid == s.appid)
            {
                continue;
            }
            out.push(ShortcutGame {
                appid: s.appid,
                name: s.name,
                dir,
            });
        }
    }
    out.sort_by_key(|g| g.name.to_lowercase());
    out
}

/// The grid artwork Steam keeps for a shortcut: portrait (`<appid>p`) first,
/// then landscape.
pub fn poster(root: &Path, appid: &str) -> Option<PathBuf> {
    let grids: Vec<PathBuf> = fs::read_dir(root.join("userdata"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("config/grid"))
        .collect();
    for stem in [format!("{appid}p"), appid.to_string()] {
        for ext in ["png", "jpg", "jpeg"] {
            for g in &grids {
                let p = g.join(format!("{stem}.{ext}"));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Shortcut `appid`'s LaunchOptions bytes and where they sit.
fn launch_options_at(root: &[Field], appid: &str) -> Option<(Vec<u8>, Range<usize>)> {
    entries(root)
        .filter(|e| appid_of(e).as_deref() == Some(appid))
        .find_map(|e| match get(e, "LaunchOptions")? {
            Val::Str(b, r) => Some((b.clone(), r.clone())),
            _ => None,
        })
}

/// The tree with byte positions dropped and shortcut `appid`'s LaunchOptions
/// blanked: two files equal here differ in nothing else.
fn normalised(f: &[Field], appid: &str) -> Vec<Field> {
    let this = appid_of(f).as_deref() == Some(appid);
    f.iter()
        .map(|x| Field {
            key: x.key.clone(),
            val: match &x.val {
                Val::Obj(inner) => Val::Obj(normalised(inner, appid)),
                Val::Str(_, _) if this && x.key.eq_ignore_ascii_case("LaunchOptions") => {
                    Val::Str(Vec::new(), 0..0)
                }
                Val::Str(b, _) => Val::Str(b.clone(), 0..0),
                Val::Int(n) => Val::Int(*n),
            },
        })
        .collect()
}

/// Current LaunchOptions of shortcut `appid`, per Steam user that has it.
pub fn read_launch_options(root: &Path, appid: &str) -> Vec<(PathBuf, Option<String>)> {
    files(root)
        .into_iter()
        .filter_map(|p| {
            let tree = parse(&fs::read(&p).ok()?).ok()?;
            let (b, _) = launch_options_at(&tree, appid)?;
            Some((p, Some(String::from_utf8_lossy(&b).into_owned())))
        })
        .collect()
}

/// Merge the required options into the shortcut's LaunchOptions. Refuses
/// while Steam runs. See `apply_with` for the mechanics.
pub fn apply_launch_options(
    root: &Path,
    appid: &str,
    req: &LaunchReq,
) -> Result<Vec<ApplyOutcome>> {
    apply_with(root, appid, req, super::steam::is_running(), false)
}

pub fn revert_launch_options(
    root: &Path,
    appid: &str,
    req: &LaunchReq,
) -> Result<Vec<ApplyOutcome>> {
    apply_with(root, appid, req, super::steam::is_running(), true)
}

/// The guarded edit, per Steam user whose shortcuts.vdf holds `appid`: splice
/// the one value, verify the result re-parses to the same tree apart from it,
/// back the file up (`.dlss5o.orig` once ever, `.dlss5o.bak` per edit) and swap
/// it in atomically. Any failure leaves the original file untouched.
pub fn apply_with(
    root: &Path,
    appid: &str,
    req: &LaunchReq,
    steam_running: bool,
    revert: bool,
) -> Result<Vec<ApplyOutcome>> {
    if steam_running {
        bail!(
            "Steam is running — it would overwrite the change on exit. \
             Close Steam and retry, or paste the options into the game's Properties yourself."
        );
    }
    let mut out = Vec::new();
    let mut found = false;
    for file in files(root) {
        let data = fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?;
        let tree = parse(&data).with_context(|| format!("cannot parse {}", file.display()))?;
        let Some((cur, span)) = launch_options_at(&tree, appid) else {
            continue;
        };
        found = true;
        let Ok(existing) = std::str::from_utf8(&cur) else {
            bail!("launch options of shortcut {appid} in {} are not UTF-8", file.display());
        };
        let merged = if revert {
            launch_options::strip(existing, req)
        } else {
            launch_options::merge(existing, req)
        };
        if merged == existing {
            continue;
        }
        let mut new_data = Vec::with_capacity(data.len() + merged.len());
        new_data.extend_from_slice(&data[..span.start]);
        new_data.extend_from_slice(merged.as_bytes());
        new_data.extend_from_slice(&data[span.end..]);
        let new_tree = parse(&new_data).context("edited file does not re-parse")?;
        if launch_options_at(&new_tree, appid).map(|(b, _)| b).as_deref() != Some(merged.as_bytes())
            || normalised(&tree, appid) != normalised(&new_tree, appid)
        {
            bail!(
                "edit self-check failed for {} — file left untouched",
                file.display()
            );
        }
        let orig = file.with_extension("vdf.dlss5o.orig");
        if !orig.exists() {
            fs::copy(&file, &orig)?;
        }
        let bak = file.with_extension("vdf.dlss5o.bak");
        fs::copy(&file, &bak)?;
        let tmp = file.with_extension("vdf.dlss5o.tmp");
        fs::write(&tmp, &new_data)?;
        fs::rename(&tmp, &file)?;
        out.push(ApplyOutcome {
            file,
            backup: bak,
            merged,
        });
    }
    if !found {
        bail!(
            "no shortcuts.vdf under {} has launch options for shortcut {appid}",
            root.display()
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(out: &mut Vec<u8>, t: u8, k: &str) {
        out.push(t);
        out.extend_from_slice(k.as_bytes());
        out.push(0);
    }

    fn string(out: &mut Vec<u8>, k: &str, v: &str) {
        key(out, T_STRING, k);
        out.extend_from_slice(v.as_bytes());
        out.push(0);
    }

    /// A shortcuts.vdf laid out the way Steam writes it:
    /// (appid, name, exe, launch options) per entry.
    fn file(list: &[(u32, &str, &str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        key(&mut out, T_OBJECT, "shortcuts");
        for (i, (appid, name, exe, launch)) in list.iter().enumerate() {
            key(&mut out, T_OBJECT, &i.to_string());
            key(&mut out, T_INT32, "appid");
            out.extend_from_slice(&appid.to_le_bytes());
            string(&mut out, "AppName", name);
            string(&mut out, "Exe", &format!("\"{exe}\""));
            string(&mut out, "StartDir", "");
            string(&mut out, "LaunchOptions", launch);
            key(&mut out, T_INT32, "LastPlayTime");
            out.extend_from_slice(&1_787_074_154u32.to_le_bytes());
            key(&mut out, T_OBJECT, "tags");
            out.push(T_END);
            out.push(T_END);
        }
        out.push(T_END);
        out.push(T_END);
        out
    }

    fn write_user(root: &Path, uid: &str, data: &[u8]) -> PathBuf {
        let f = root.join("userdata").join(uid).join("config/shortcuts.vdf");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, data).unwrap();
        f
    }

    fn dxgi_req() -> LaunchReq {
        LaunchReq {
            overrides: vec![("dxgi".into(), "n,b".into())],
            env: vec![("PROTON_ENABLE_NVAPI".into(), "1".into())],
        }
    }

    #[test]
    fn parses_high_appids_and_quoted_exes() {
        let d = file(&[
            (3_466_011_274, "Pragmata", "/g/PRAGMATA/PRAGMATA.exe", ""),
            (2_194_695_776, "Stellar Blade", "/g/SB/Win64/SB.exe", "MANGOHUD=1 %command%"),
        ]);
        let v = parse_shortcuts(&d).unwrap();
        assert_eq!(v.len(), 2);
        // Above i32::MAX: the unsigned form, as the compatdata folder is named.
        assert_eq!(v[0].appid, "3466011274");
        assert_eq!(v[0].dir(), Some(PathBuf::from("/g/PRAGMATA")));
        assert!(v[0].is_windows_exe());
        assert_eq!(v[1].launch_options, "MANGOHUD=1 %command%");
    }

    #[test]
    fn native_programs_are_not_windows_exes() {
        let s = Shortcut {
            appid: "1".into(),
            name: "Moonlight".into(),
            exe: "\"/usr/bin/moonlight\"".into(),
            launch_options: String::new(),
        };
        assert_eq!(s.dir(), Some(PathBuf::from("/usr/bin")));
        assert!(!s.is_windows_exe());
    }

    #[test]
    fn truncated_and_unknown_types_are_errors() {
        let d = file(&[(1, "A", "/g/a.exe", "")]);
        assert!(parse(&d[..d.len() - 3]).is_err());
        let mut bad = d.clone();
        bad[0] = 0x05;
        assert!(parse(&bad).is_err());
    }

    #[test]
    fn apply_splices_one_value_keeps_backups_and_reverts() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Steam");
        let d = file(&[
            (2_194_695_776, "Stellar Blade", "/g/sb.exe", ""),
            (3_466_011_274, "Pragmata", "/g/p.exe", ""),
        ]);
        let f = write_user(&root, "111", &d);

        let out = apply_with(&root, "3466011274", &dxgi_req(), false, false).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].merged,
            "WINEDLLOVERRIDES=\"dxgi=n,b\" PROTON_ENABLE_NVAPI=1 %command%"
        );
        assert!(f.with_extension("vdf.dlss5o.orig").is_file());
        assert!(f.with_extension("vdf.dlss5o.bak").is_file());
        // Every byte outside the one value is unchanged.
        let now = fs::read(&f).unwrap();
        let (_, span) = launch_options_at(&parse(&d).unwrap(), "3466011274").unwrap();
        assert_eq!(&now[..span.start], &d[..span.start]);
        assert_eq!(&now[span.start + out[0].merged.len()..], &d[span.end..]);
        assert_eq!(
            read_launch_options(&root, "3466011274")[0].1.as_deref(),
            Some(out[0].merged.as_str())
        );
        assert_eq!(read_launch_options(&root, "2194695776")[0].1.as_deref(), Some(""));

        // Second apply: nothing to do. Revert: the original file, byte for byte.
        assert!(apply_with(&root, "3466011274", &dxgi_req(), false, false)
            .unwrap()
            .is_empty());
        apply_with(&root, "3466011274", &dxgi_req(), false, true).unwrap();
        assert_eq!(fs::read(&f).unwrap(), d);
    }

    #[test]
    fn apply_keeps_the_users_own_tokens() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Steam");
        write_user(&root, "111", &file(&[(7, "G", "/g/g.exe", "MANGOHUD=1 %command% -dx12")]));
        let out = apply_with(&root, "7", &dxgi_req(), false, false).unwrap();
        assert!(out[0].merged.starts_with("MANGOHUD=1 "), "{}", out[0].merged);
        assert!(out[0].merged.ends_with("%command% -dx12"), "{}", out[0].merged);
    }

    #[test]
    fn apply_refuses_while_steam_runs_or_for_unknown_shortcuts() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Steam");
        let d = file(&[(7, "G", "/g/g.exe", "")]);
        let f = write_user(&root, "111", &d);
        assert!(apply_with(&root, "7", &dxgi_req(), true, false).is_err());
        assert!(apply_with(&root, "8", &dxgi_req(), false, false).is_err());
        assert_eq!(fs::read(&f).unwrap(), d);
    }

    #[test]
    fn apply_leaves_a_malformed_file_untouched() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Steam");
        let d = file(&[(7, "G", "/g/g.exe", "")]);
        let broken = &d[..d.len() - 3];
        let f = write_user(&root, "111", broken);
        assert!(apply_with(&root, "7", &dxgi_req(), false, false).is_err());
        assert_eq!(fs::read(&f).unwrap(), broken);
        assert!(!f.with_extension("vdf.dlss5o.orig").exists());
    }

    #[test]
    fn games_skip_native_programs_missing_folders_and_duplicates() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Steam");
        let game = t.path().join("games/A");
        fs::create_dir_all(&game).unwrap();
        let exe = game.join("a.exe");
        let gone = t.path().join("nope/x.exe");
        let exe = exe.to_str().unwrap();
        write_user(
            &root,
            "111",
            &file(&[
                (1, "A", exe, ""),
                (2, "Moonlight", "/usr/bin/moonlight", ""),
                (3, "Gone", gone.to_str().unwrap(), ""),
            ]),
        );
        write_user(&root, "222", &file(&[(1, "A", exe, "")]));
        let got = games(&root);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].appid, "1");
        assert_eq!(got[0].dir, game);
    }

    #[test]
    fn poster_prefers_portrait_grid_art() {
        let t = tempfile::tempdir().unwrap();
        let grid = t.path().join("userdata/111/config/grid");
        fs::create_dir_all(&grid).unwrap();
        fs::write(grid.join("5.jpg"), b"x").unwrap();
        fs::write(grid.join("5p.png"), b"x").unwrap();
        assert_eq!(poster(t.path(), "5"), Some(grid.join("5p.png")));
        assert_eq!(poster(t.path(), "6"), None);
    }
}
