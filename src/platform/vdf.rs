//! Minimal Valve KeyValues ("VDF") reader plus a byte-preserving editor.
//!
//! `libraryfolders.vdf`, `appmanifest_*.acf`, `config.vdf` and `localconfig.vdf`
//! are all this format: `"key" "value"` pairs and `"key" { … }` blocks, quoted
//! strings with `\" \\ \n \t` escapes, `//` line comments, tab indentation.
//! Reading builds an ordered tree (duplicate keys kept, lookups case-insensitive
//! because Valve's own casing varies). Editing never re-serialises the tree:
//! `set_string_preserving` splices exactly one value into the original text so
//! a file Steam also rewrites (localconfig.vdf) keeps every other byte intact.

use anyhow::{bail, Result};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Block(Block),
}

/// Ordered key/value pairs; duplicate keys preserved.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block(pub Vec<(String, Value)>);

impl Block {
    pub fn get_ci(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }
    pub fn path_ci(&self, path: &[&str]) -> Option<&Value> {
        let mut cur = self.get_ci(path.first()?)?;
        for key in &path[1..] {
            match cur {
                Value::Block(b) => cur = b.get_ci(key)?,
                Value::Str(_) => return None,
            }
        }
        Some(cur)
    }
    pub fn string_at(&self, path: &[&str]) -> Option<&str> {
        match self.path_ci(path)? {
            Value::Str(s) => Some(s),
            Value::Block(_) => None,
        }
    }
}

// ── tokenizer ──────────────────────────────────────────────────────

#[derive(Debug)]
enum Tok {
    /// Quoted or bare string; `raw` spans the source bytes (quotes included).
    Str { raw: Range<usize>, val: String },
    Open(usize),
    Close(usize),
}

fn unescape_into(out: &mut String, c: char) {
    match c {
        'n' => out.push('\n'),
        't' => out.push('\t'),
        '\\' => out.push('\\'),
        '"' => out.push('"'),
        other => {
            // Unknown escape: keep both characters, like Valve's lenient parser.
            out.push('\\');
            out.push(other);
        }
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

fn tokenize(text: &str) -> Result<Vec<Tok>> {
    let b = text.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        match c {
            b'{' => {
                toks.push(Tok::Open(i));
                i += 1;
            }
            b'}' => {
                toks.push(Tok::Close(i));
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut val = String::new();
                loop {
                    let Some(&ch) = b.get(i) else {
                        bail!("unterminated string at byte {start}");
                    };
                    match ch {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => {
                            let Some(&esc) = b.get(i + 1) else {
                                bail!("dangling escape at byte {i}");
                            };
                            unescape_into(&mut val, esc as char);
                            i += 2;
                        }
                        _ => {
                            // Copy the full UTF-8 character, not just one byte.
                            let s = &text[i..];
                            let ch = s.chars().next().unwrap();
                            val.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                toks.push(Tok::Str {
                    raw: start..i,
                    val,
                });
            }
            _ => {
                // Bare token (rare in Steam files, but legal).
                let start = i;
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && b[i] != b'{'
                    && b[i] != b'}'
                    && b[i] != b'"'
                {
                    i += 1;
                }
                toks.push(Tok::Str {
                    raw: start..i,
                    val: text[start..i].to_string(),
                });
            }
        }
    }
    Ok(toks)
}

// ── spanned parse (internal) ───────────────────────────────────────

#[derive(Debug)]
enum SpannedKind {
    Str { raw: Range<usize>, val: String },
    Block { inner: SpannedBlock, close: usize },
}

#[derive(Debug, Default)]
struct SpannedBlock(Vec<(String, SpannedKind)>);

impl SpannedBlock {
    fn get_ci(&self, key: &str) -> Option<&SpannedKind> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }
    fn strip(&self) -> Block {
        Block(
            self.0
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        match v {
                            SpannedKind::Str { val, .. } => Value::Str(val.clone()),
                            SpannedKind::Block { inner, .. } => Value::Block(inner.strip()),
                        },
                    )
                })
                .collect(),
        )
    }
}

fn parse_block(toks: &[Tok], pos: &mut usize, top: bool) -> Result<(SpannedBlock, usize)> {
    let mut out = SpannedBlock::default();
    let mut close = usize::MAX;
    loop {
        match toks.get(*pos) {
            None => {
                if top {
                    return Ok((out, close));
                }
                bail!("unexpected end of file inside a block");
            }
            Some(Tok::Close(at)) => {
                if top {
                    bail!("stray '}}' at byte {at}");
                }
                close = *at;
                *pos += 1;
                return Ok((out, close));
            }
            Some(Tok::Open(at)) => bail!("'{{' without a key at byte {at}"),
            Some(Tok::Str { raw, val }) => {
                let key = val.clone();
                let _key_raw = raw.clone();
                *pos += 1;
                match toks.get(*pos) {
                    Some(Tok::Str { raw, val }) => {
                        out.0.push((
                            key,
                            SpannedKind::Str {
                                raw: raw.clone(),
                                val: val.clone(),
                            },
                        ));
                        *pos += 1;
                    }
                    Some(Tok::Open(_)) => {
                        *pos += 1;
                        let (inner, close) = parse_block(toks, pos, false)?;
                        out.0.push((key, SpannedKind::Block { inner, close }));
                    }
                    _ => bail!("key {key:?} has no value"),
                }
            }
        }
    }
}

fn parse_spanned(text: &str) -> Result<SpannedBlock> {
    let toks = tokenize(text)?;
    let mut pos = 0usize;
    let (root, _) = parse_block(&toks, &mut pos, true)?;
    Ok(root)
}

pub fn parse(text: &str) -> Result<Block> {
    Ok(parse_spanned(text)?.strip())
}

// ── surgical editing ───────────────────────────────────────────────

/// Replace or insert ONE string value at `path` (all components matched
/// case-insensitively), preserving every other byte of `text`. Creates at most
/// the last two levels — a missing final key inside an existing parent block,
/// or a missing parent block (with the final key inside) in an existing
/// grandparent. Anything shallower missing is an error; callers fall back to
/// telling the user instead of guessing at file structure.
pub fn set_string_preserving(text: &str, path: &[&str], value: &str) -> Result<String> {
    if path.is_empty() {
        bail!("empty path");
    }
    let root = parse_spanned(text)?;

    // Walk as far as the tree goes.
    let mut blocks: Vec<&SpannedBlock> = vec![&root];
    let mut depth = 0usize; // how many path components resolved to blocks
    while depth < path.len() - 1 {
        match blocks[depth].get_ci(path[depth]) {
            Some(SpannedKind::Block { inner, .. }) => {
                blocks.push(inner);
                depth += 1;
            }
            Some(SpannedKind::Str { .. }) => {
                bail!("{:?} is a value, not a block", path[depth])
            }
            None => break,
        }
    }

    let quoted = format!("\"{}\"", escape(value));
    if depth == path.len() - 1 {
        // Parent block exists.
        match blocks[depth].get_ci(path[depth]) {
            Some(SpannedKind::Str { raw, .. }) => {
                // Replace just the quoted value bytes.
                let mut out = String::with_capacity(text.len() + quoted.len());
                out.push_str(&text[..raw.start]);
                out.push_str(&quoted);
                out.push_str(&text[raw.end..]);
                return Ok(out);
            }
            Some(SpannedKind::Block { .. }) => {
                bail!("{:?} is a block, not a value", path[depth])
            }
            None => {
                // Insert `"key"\t\t"value"` before the parent's closing brace.
                let close = parent_close(&root, path, depth)?;
                let indent = "\t".repeat(depth);
                let line = format!("{indent}\"{}\"\t\t{}\n", escape(path[depth]), quoted);
                return Ok(insert_before_line(text, close, &line));
            }
        }
    }
    if depth == path.len() - 2 {
        // Grandparent exists; create the parent block with the final key inside.
        let close = parent_close(&root, path, depth)?;
        let indent = "\t".repeat(depth);
        let block = format!(
            "{indent}\"{}\"\n{indent}{{\n{indent}\t\"{}\"\t\t{}\n{indent}}}\n",
            escape(path[depth]),
            escape(path[depth + 1]),
            quoted
        );
        return Ok(insert_before_line(text, close, &block));
    }
    bail!(
        "cannot create {:?}: {:?} not found",
        path.join(">"),
        path[depth]
    )
}

/// Byte offset of the closing brace of the block reached after resolving
/// `path[..depth]` (depth ≥ 1); the top level has no brace and is an error.
fn parent_close(root: &SpannedBlock, path: &[&str], depth: usize) -> Result<usize> {
    if depth == 0 {
        bail!("refusing to create top-level {:?}", path[0]);
    }
    let mut cur = root;
    let mut close = None;
    for key in &path[..depth] {
        match cur.get_ci(key) {
            Some(SpannedKind::Block { inner, close: c }) => {
                close = Some(*c);
                cur = inner;
            }
            _ => bail!("{key:?} not found"),
        }
    }
    Ok(close.expect("depth >= 1"))
}

/// Insert `what` at the start of the line containing byte `at`.
fn insert_before_line(text: &str, at: usize, what: &str) -> String {
    let line_start = text[..at].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let mut out = String::with_capacity(text.len() + what.len());
    out.push_str(&text[..line_start]);
    out.push_str(what);
    out.push_str(&text[line_start..]);
    out
}

/// True when `a` and `b` are the same tree except possibly the string value at
/// `path` — the post-edit safety check for `set_string_preserving`.
pub fn equal_except(a: &Block, b: &Block, path: &[&str]) -> bool {
    fn eq(a: &Block, b: &Block, path: &[&str]) -> bool {
        if a.0.len() != b.0.len() {
            // The edit may have inserted the final key or the parent block.
            return matches!(path, [_] | [_, _]) && inserted_ok(a, b, path);
        }
        a.0.iter().zip(b.0.iter()).all(|((ka, va), (kb, vb))| {
            if !ka.eq_ignore_ascii_case(kb) {
                return false;
            }
            match (va, vb, path) {
                (Value::Str(sa), Value::Str(sb), [last]) if ka.eq_ignore_ascii_case(last) => {
                    let _ = (sa, sb); // the one value allowed to differ
                    true
                }
                (Value::Block(ba), Value::Block(bb), [head, rest @ ..])
                    if ka.eq_ignore_ascii_case(head) && !rest.is_empty() =>
                {
                    eq(ba, bb, rest)
                }
                _ => va == vb,
            }
        })
    }
    /// `b` = `a` plus exactly the inserted entry named by `path`.
    fn inserted_ok(a: &Block, b: &Block, path: &[&str]) -> bool {
        if b.0.len() != a.0.len() + 1 {
            return false;
        }
        let mut extra = None;
        let mut ai = a.0.iter().peekable();
        for (kb, vb) in &b.0 {
            match ai.peek() {
                Some((ka, va)) if ka.eq_ignore_ascii_case(kb) && {
                    // same key: must be identical entry
                    *va == *vb
                } =>
                {
                    ai.next();
                }
                _ => {
                    if extra.replace((kb, vb)).is_some() {
                        return false;
                    }
                }
            }
        }
        if ai.next().is_some() {
            return false;
        }
        let Some((kb, vb)) = extra else { return false };
        if !kb.eq_ignore_ascii_case(path[0]) {
            return false;
        }
        match (vb, path) {
            (Value::Str(_), [_]) => true,
            (Value::Block(inner), [_, last]) => {
                inner.0.len() == 1
                    && inner.0[0].0.eq_ignore_ascii_case(last)
                    && matches!(inner.0[0].1, Value::Str(_))
            }
            _ => false,
        }
    }
    eq(a, b, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\"UserLocalConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"1091500\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LaunchOptions\"\t\t\"DXVK_NVAPI_DRIVER_VERSION=53742 WINEDLLOVERRIDES=\\\"d3dcompiler_47=n;dxgi=n,b\\\" PROTON_ENABLE_NVAPI=1 %command%\"\n\t\t\t\t\t\t\"Playtime\"\t\t\"100\"\n\t\t\t\t\t}\n\t\t\t\t\t\"489830\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"Playtime\"\t\t\"7\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n\t\"friends\"\n\t{\n\t\t\"x\"\t\t\"1\"\n\t}\n}\n";

    const APPS: [&str; 5] = ["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"];

    fn path_for(appid: &str) -> Vec<&str> {
        let mut p: Vec<&str> = APPS.to_vec();
        p.push(appid);
        p.push("LaunchOptions");
        p
    }

    #[test]
    fn parses_nested_blocks_and_escaped_quotes() {
        let root = parse(SAMPLE).unwrap();
        let lo = root.string_at(&path_for("1091500")).unwrap();
        assert!(lo.contains(r#"WINEDLLOVERRIDES="d3dcompiler_47=n;dxgi=n,b""#));
        assert!(lo.ends_with("%command%"));
        assert_eq!(root.string_at(&path_for("489830")), None);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let root = parse(SAMPLE).unwrap();
        assert!(root
            .string_at(&[
                "userlocalconfigstore",
                "SOFTWARE",
                "valve",
                "STEAM",
                "Apps",
                "1091500",
                "launchoptions"
            ])
            .is_some());
    }

    #[test]
    fn duplicate_keys_are_kept() {
        let root = parse("\"a\"\t\"1\"\n\"a\"\t\"2\"\n").unwrap();
        assert_eq!(root.0.len(), 2);
    }

    #[test]
    fn comments_and_bare_tokens() {
        let root = parse("// header\n\"k\" { bare token }\n").unwrap();
        let Value::Block(b) = root.get_ci("k").unwrap() else {
            panic!()
        };
        assert_eq!(b.0[0], ("bare".into(), Value::Str("token".into())));
    }

    #[test]
    fn replace_touches_only_the_value_bytes() {
        let path = path_for("1091500");
        let out = set_string_preserving(SAMPLE, &path, "NEW %command%").unwrap();
        // Everything before and after the old quoted value is byte-identical.
        let old_pos = SAMPLE.find("\"DXVK_NVAPI").unwrap();
        assert_eq!(&out[..old_pos], &SAMPLE[..old_pos]);
        assert!(out.contains("\"NEW %command%\""));
        let tail = "\"Playtime\"\t\t\"100\"";
        assert_eq!(
            out[out.find(tail).unwrap()..],
            SAMPLE[SAMPLE.find(tail).unwrap()..]
        );
        let reparsed = parse(&out).unwrap();
        assert_eq!(reparsed.string_at(&path), Some("NEW %command%"));
        assert!(equal_except(&parse(SAMPLE).unwrap(), &reparsed, &path));
    }

    #[test]
    fn escapes_quotes_when_writing() {
        let path = path_for("1091500");
        let out =
            set_string_preserving(SAMPLE, &path, r#"WINEDLLOVERRIDES="dxgi=n,b" %command%"#)
                .unwrap();
        assert!(out.contains(r#"\"dxgi=n,b\""#));
        assert_eq!(
            parse(&out).unwrap().string_at(&path),
            Some(r#"WINEDLLOVERRIDES="dxgi=n,b" %command%"#)
        );
    }

    #[test]
    fn inserts_missing_launch_options_key() {
        let path = path_for("489830");
        let out = set_string_preserving(SAMPLE, &path, "X %command%").unwrap();
        let reparsed = parse(&out).unwrap();
        assert_eq!(reparsed.string_at(&path), Some("X %command%"));
        // Sibling data untouched.
        assert_eq!(
            reparsed.string_at(&[
                "UserLocalConfigStore",
                "Software",
                "Valve",
                "Steam",
                "apps",
                "489830",
                "Playtime"
            ]),
            Some("7")
        );
        assert!(equal_except(&parse(SAMPLE).unwrap(), &reparsed, &path));
    }

    #[test]
    fn inserts_missing_appid_block() {
        let path = path_for("999999");
        let out = set_string_preserving(SAMPLE, &path, "Y %command%").unwrap();
        let reparsed = parse(&out).unwrap();
        assert_eq!(reparsed.string_at(&path), Some("Y %command%"));
        assert!(equal_except(&parse(SAMPLE).unwrap(), &reparsed, &path));
    }

    #[test]
    fn refuses_when_apps_block_missing() {
        let text = "\"UserLocalConfigStore\"\n{\n}\n";
        let path = path_for("1");
        assert!(set_string_preserving(text, &path, "v").is_err());
    }

    #[test]
    fn equal_except_rejects_unrelated_changes() {
        let path = path_for("1091500");
        let a = parse(SAMPLE).unwrap();
        let mangled = SAMPLE.replace("\"Playtime\"\t\t\"100\"", "\"Playtime\"\t\t\"999\"");
        let b = parse(&mangled).unwrap();
        assert!(!equal_except(&a, &b, &path));
    }

    #[test]
    fn roundtrip_unescape_escape() {
        let s = r#"a "quoted" \back\ and	tab"#;
        let text = format!("\"k\"\t\"{}\"\n", escape(s));
        assert_eq!(parse(&text).unwrap().string_at(&["k"]), Some(s));
    }
}
