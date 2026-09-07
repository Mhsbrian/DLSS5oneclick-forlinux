//! "Report a bug": open a GitHub issue for this fork with the facts already
//! filled in — tool version, card, driver, game, route, the last diagnosis and
//! log tails — so a report is useful without the user assembling any of it.
//! Nothing is sent on its own: the browser opens the prefilled page and the
//! user decides. All of it is editable before they submit.

use std::path::Path;

/// This fork's issue tracker.
pub const REPORT_REPO: &str = "Mhsbrian/DLSS5oneclick-forlinux";

/// The facts a report carries. The GUI fills these from the current game.
#[derive(Default)]
pub struct Report {
    pub version: String,
    pub game: String,
    pub gpu: String,
    pub driver: String,
    pub api: String,
    pub route: String,
    pub diagnosis: Vec<String>,
    /// `(filename, tail)` for each log found beside the game.
    pub logs: Vec<(String, String)>,
}

impl Report {
    /// The issue title — short, the user edits it.
    pub fn title(&self) -> String {
        let game = if self.game.is_empty() {
            "a game".to_string()
        } else {
            self.game.clone()
        };
        format!("[bug] {game}: ")
    }

    /// The issue body, GitHub-flavoured Markdown.
    pub fn body(&self) -> String {
        let mut b = String::new();
        b.push_str("_Describe what happened here._\n\n");
        b.push_str(&format!("**Tool:** dlss5oneclick {} (Linux)\n", self.version));
        b.push_str(&format!("**GPU:** {}\n", nonempty(&self.gpu, "?")));
        b.push_str(&format!("**Driver:** {}\n", nonempty(&self.driver, "?")));
        b.push_str(&format!("**Game:** {}\n", nonempty(&self.game, "?")));
        b.push_str(&format!("**API:** {}\n", nonempty(&self.api, "?")));
        b.push_str(&format!("**Route:** {}\n", nonempty(&self.route, "?")));
        if !self.diagnosis.is_empty() {
            b.push_str("\n### Diagnosis\n");
            for f in &self.diagnosis {
                b.push_str(&format!("- {f}\n"));
            }
        }
        for (name, tail) in &self.logs {
            b.push_str(&format!(
                "\n<details><summary>{name} (tail)</summary>\n\n```\n{}\n```\n</details>\n",
                tail.trim_end()
            ));
        }
        b.push_str("\n---\n_Filled in by dlss5oneclick — edit anything before submitting._\n");
        b
    }

    /// The GitHub "new issue" URL with title and body prefilled.
    pub fn url(&self) -> String {
        issue_url(&self.title(), &self.body())
    }
}

fn nonempty<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.trim().is_empty() {
        fallback
    } else {
        s
    }
}

/// Build the prefilled issue URL, clamping the body so the URL stays within what
/// browsers accept (GitHub truncates very long ones anyway).
pub fn issue_url(title: &str, body: &str) -> String {
    format!(
        "https://github.com/{REPORT_REPO}/issues/new?title={}&body={}",
        urlencode(title),
        urlencode(&clamp(body, 6000))
    )
}

/// Percent-encode for a URL query value: everything but the unreserved set.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Truncate to at most `max` bytes on a char boundary, noting the cut.
fn clamp(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n…(truncated)", &s[..end])
}

/// The last `lines` lines of a text file, or `None` when it is not there.
pub fn tail(path: &Path, lines: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    Some(all[start..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_encodes_reserved_and_keeps_unreserved() {
        assert_eq!(urlencode("a-b_c.~"), "a-b_c.~");
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a\nb"), "a%0Ab");
        assert_eq!(urlencode("100%"), "100%25");
        assert_eq!(urlencode("#&="), "%23%26%3D");
    }

    #[test]
    fn clamp_is_char_boundary_safe() {
        let s = "é".repeat(10); // 2 bytes each = 20 bytes
        let out = clamp(&s, 5);
        assert!(out.starts_with("éé")); // never splits a char
        assert!(out.contains("truncated"));
        assert_eq!(clamp("short", 100), "short");
    }

    #[test]
    fn report_url_carries_the_facts() {
        let r = Report {
            version: "0.14.0".into(),
            game: "Cyberpunk 2077".into(),
            gpu: "RTX 4090".into(),
            driver: "610.57.04".into(),
            api: "DX12".into(),
            route: "OptiScaler -> model".into(),
            diagnosis: vec!["NR evaluating".into()],
            logs: vec![("OptiScaler.log".into(), "line1\nline2".into())],
        };
        let body = r.body();
        assert!(body.contains("Cyberpunk 2077"));
        assert!(body.contains("RTX 4090"));
        assert!(body.contains("OptiScaler -> model"));
        assert!(body.contains("NR evaluating"));
        assert!(body.contains("OptiScaler.log"));
        let url = r.url();
        assert!(url.starts_with(&format!("https://github.com/{REPORT_REPO}/issues/new?title=")));
        assert!(url.contains("Cyberpunk"));
        // Title stays short; the game leads it.
        assert!(r.title().starts_with("[bug] Cyberpunk 2077:"));
    }
}
