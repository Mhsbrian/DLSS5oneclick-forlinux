//! What a Proton game's Steam launch options must contain for the installed
//! stack to load, and how to fold that into whatever the user already has.
//!
//! ReShade and OptiScaler both sit in the game folder as `dxgi.dll`; Proton
//! only loads it over its builtin with `WINEDLLOVERRIDES="dxgi=n,b"`. ReShade
//! compiles effects with a native `d3dcompiler_47.dll` when one is next to the
//! exe. NVAPI (the DLSS plumbing) is on by default since Proton 9; older or
//! unknown Proton gets `PROTON_ENABLE_NVAPI=1` (harmless when redundant).
//!
//! Everything here is pure string work, identical on every OS, so it is fully
//! unit-tested everywhere; only the callers are Linux-gated.

use crate::game;
use crate::installer::Engine;
use std::path::Path;

use super::steam::ProtonInfo;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchReq {
    /// WINEDLLOVERRIDES entries, e.g. ("dxgi", "n,b").
    pub overrides: Vec<(String, String)>,
    /// Plain environment variables, e.g. ("PROTON_ENABLE_NVAPI", "1").
    pub env: Vec<(String, String)>,
}

/// The options this game needs with the given engine and Proton build.
pub fn required(game_dir: &Path, engine: Engine, proton: Option<&ProtonInfo>) -> LaunchReq {
    let mut req = LaunchReq::default();
    if engine == Engine::ReShade && d3dcompiler_present(game_dir) {
        req.overrides
            .push(("d3dcompiler_47".into(), "n".into()));
    }
    req.overrides.push(("dxgi".into(), "n,b".into()));
    if super::steam::nvapi_env_needed(proton) {
        req.env.push(("PROTON_ENABLE_NVAPI".into(), "1".into()));
    }
    req
}

pub fn d3dcompiler_present(game_dir: &Path) -> bool {
    game::existing_ci(game_dir, "d3dcompiler_47.dll").is_some_and(|p| p.is_file())
}

/// The same requirements as plain environment variables, for launchers that
/// take per-game env settings instead of a Steam-style command line.
pub fn env_pairs(req: &LaunchReq) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    if !req.overrides.is_empty() {
        let inner: Vec<String> = req
            .overrides
            .iter()
            .map(|(n, m)| {
                if m.is_empty() {
                    n.clone()
                } else {
                    format!("{n}={m}")
                }
            })
            .collect();
        vars.push(("WINEDLLOVERRIDES".into(), inner.join(";")));
    }
    vars.extend(req.env.iter().cloned());
    vars
}

/// The canonical full string for an empty starting point,
/// e.g. `WINEDLLOVERRIDES="d3dcompiler_47=n;dxgi=n,b" PROTON_ENABLE_NVAPI=1 %command%`.
pub fn display(req: &LaunchReq) -> String {
    merge("", req)
}

/// Fold `req` into an existing launch-options string. Idempotent; preserves
/// every token the user wrote (their env vars, their WINEDLLOVERRIDES entries
/// for other DLLs, game arguments after `%command%`). A string with no
/// `%command%` is plain game arguments — those move behind an inserted one.
pub fn merge(existing: &str, req: &LaunchReq) -> String {
    let toks = split_tokens(existing);
    let cmd = toks.iter().position(|t| t == "%command%");
    let (mut pre, post): (Vec<String>, Vec<String>) = match cmd {
        Some(i) => (toks[..i].to_vec(), toks[i + 1..].to_vec()),
        None => (Vec::new(), toks),
    };
    if !req.overrides.is_empty() {
        let mut placed = false;
        for t in pre.iter_mut() {
            if let Some(v) = t.strip_prefix("WINEDLLOVERRIDES=") {
                let mut entries = parse_overrides(v.trim_matches('"'));
                for (n, m) in &req.overrides {
                    upsert(&mut entries, n, m);
                }
                *t = fmt_overrides_token(&entries);
                placed = true;
                break;
            }
        }
        if !placed {
            let mut entries = Vec::new();
            for (n, m) in &req.overrides {
                upsert(&mut entries, n, m);
            }
            pre.push(fmt_overrides_token(&entries));
        }
    }
    for (name, value) in &req.env {
        let pat = format!("{name}=");
        match pre.iter_mut().find(|t| t.starts_with(&pat)) {
            Some(t) => *t = format!("{name}={value}"),
            None => pre.push(format!("{name}={value}")),
        }
    }
    let mut out = pre;
    out.push("%command%".into());
    out.extend(post);
    out.join(" ")
}

/// Remove exactly what `merge` with the same `req` would add. Foreign tokens
/// stay; a result of just `%command%` collapses to the empty string.
pub fn strip(existing: &str, req: &LaunchReq) -> String {
    let toks = split_tokens(existing);
    let Some(cmd) = toks.iter().position(|t| t == "%command%") else {
        return existing.trim().to_string(); // nothing of ours without %command%
    };
    let mut pre: Vec<String> = toks[..cmd].to_vec();
    let post = &toks[cmd + 1..];
    pre.retain(|t| {
        !req.env
            .iter()
            .any(|(n, _)| t.starts_with(&format!("{n}=")))
    });
    let mut drop_idx = None;
    for (i, t) in pre.iter_mut().enumerate() {
        if let Some(v) = t.strip_prefix("WINEDLLOVERRIDES=") {
            let mut entries = parse_overrides(v.trim_matches('"'));
            entries.retain(|(n, _)| !req.overrides.iter().any(|(rn, _)| rn == n));
            if entries.is_empty() {
                drop_idx = Some(i);
            } else {
                *t = fmt_overrides_token(&entries);
            }
            break;
        }
    }
    if let Some(i) = drop_idx {
        pre.remove(i);
    }
    if pre.is_empty() && post.is_empty() {
        return String::new();
    }
    let mut out = pre;
    out.push("%command%".into());
    out.extend(post.iter().cloned());
    out.join(" ")
}

/// Split on whitespace, but keep double-quoted stretches inside one token.
fn split_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            c if c.is_whitespace() && !in_q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_overrides(v: &str) -> Vec<(String, String)> {
    v.split(';')
        .filter_map(|e| {
            let e = e.trim();
            if e.is_empty() {
                return None;
            }
            match e.split_once('=') {
                Some((n, m)) => Some((n.trim().to_string(), m.trim().to_string())),
                None => Some((e.to_string(), String::new())),
            }
        })
        .collect()
}

fn upsert(entries: &mut Vec<(String, String)>, name: &str, mode: &str) {
    match entries.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        Some(e) => e.1 = mode.to_string(),
        None => entries.push((name.to_string(), mode.to_string())),
    }
}

fn fmt_overrides_token(entries: &[(String, String)]) -> String {
    let inner: Vec<String> = entries
        .iter()
        .map(|(n, m)| {
            if m.is_empty() {
                n.clone()
            } else {
                format!("{n}={m}")
            }
        })
        .collect();
    format!("WINEDLLOVERRIDES=\"{}\"", inner.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(d3d: bool, nvapi: bool) -> LaunchReq {
        let mut r = LaunchReq::default();
        if d3d {
            r.overrides.push(("d3dcompiler_47".into(), "n".into()));
        }
        r.overrides.push(("dxgi".into(), "n,b".into()));
        if nvapi {
            r.env.push(("PROTON_ENABLE_NVAPI".into(), "1".into()));
        }
        r
    }

    #[test]
    fn display_matches_convention() {
        assert_eq!(
            display(&req(true, true)),
            "WINEDLLOVERRIDES=\"d3dcompiler_47=n;dxgi=n,b\" PROTON_ENABLE_NVAPI=1 %command%"
        );
        assert_eq!(
            display(&req(false, false)),
            "WINEDLLOVERRIDES=\"dxgi=n,b\" %command%"
        );
    }

    #[test]
    fn merge_is_a_noop_on_this_rigs_existing_options() {
        // Verbatim from the machine's localconfig.vdf.
        let existing = "DXVK_NVAPI_DRIVER_VERSION=53742 WINEDLLOVERRIDES=\"d3dcompiler_47=n;dxgi=n,b\" PROTON_ENABLE_NVAPI=1 %command%";
        assert_eq!(merge(existing, &req(true, true)), existing);
        let existing2 = "PROTON_ENABLE_NVAPI=1 WINEDLLOVERRIDES=\"dxgi=n,b\" %command%";
        assert_eq!(merge(existing2, &req(false, true)), existing2);
    }

    #[test]
    fn merge_preserves_user_tokens_and_game_args() {
        let out = merge("MANGOHUD=1 %command% -dx11 --skip-intro", &req(false, true));
        assert_eq!(
            out,
            "MANGOHUD=1 WINEDLLOVERRIDES=\"dxgi=n,b\" PROTON_ENABLE_NVAPI=1 %command% -dx11 --skip-intro"
        );
    }

    #[test]
    fn merge_folds_into_existing_overrides() {
        let out = merge(
            "WINEDLLOVERRIDES=\"winmm=n,b\" %command%",
            &req(true, false),
        );
        assert_eq!(
            out,
            "WINEDLLOVERRIDES=\"winmm=n,b;d3dcompiler_47=n;dxgi=n,b\" %command%"
        );
    }

    #[test]
    fn merge_overwrites_conflicting_mode() {
        let out = merge("WINEDLLOVERRIDES=\"dxgi=b\" %command%", &req(false, false));
        assert_eq!(out, "WINEDLLOVERRIDES=\"dxgi=n,b\" %command%");
    }

    #[test]
    fn plain_args_move_behind_command() {
        let out = merge("-windowed", &req(false, false));
        assert_eq!(out, "WINEDLLOVERRIDES=\"dxgi=n,b\" %command% -windowed");
    }

    #[test]
    fn double_merge_is_idempotent() {
        let r = req(true, true);
        let once = merge("GAMEMODE=1 %command% -foo", &r);
        assert_eq!(merge(&once, &r), once);
    }

    #[test]
    fn strip_removes_only_ours() {
        let r = req(true, true);
        let merged = merge(
            "MANGOHUD=1 WINEDLLOVERRIDES=\"winmm=n,b\" %command% -dx11",
            &r,
        );
        assert_eq!(
            strip(&merged, &r),
            "MANGOHUD=1 WINEDLLOVERRIDES=\"winmm=n,b\" %command% -dx11"
        );
    }

    #[test]
    fn strip_collapses_to_empty() {
        let r = req(true, true);
        assert_eq!(strip(&display(&r), &r), "");
    }

    #[test]
    fn quoted_values_with_spaces_stay_one_token() {
        let toks = split_tokens("A=\"x y\" %command%");
        assert_eq!(toks, vec!["A=\"x y\"", "%command%"]);
    }
}
