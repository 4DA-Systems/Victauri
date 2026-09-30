//! Neutralize untrusted text before it reaches a terminal or a CI log.
//!
//! Page-controlled strings (console messages, uncaught-error text, IPC command names) and
//! project-controlled strings (`tauri.conf.json` identifiers, capability window labels, file
//! names) are printed by `victauri check` / `test` / `doctor` / `init`. Raw control characters
//! in them can move the cursor, recolor or erase output (ANSI escapes), or — on CI — start a
//! new line reading `::error ...` that GitHub Actions turns into a forged annotation
//! (round-4 audit R4-TERM1). These helpers render every such character visibly instead.

use std::fmt::Write;

/// Bidirectional-override and isolate controls, which reorder how the surrounding text is
/// displayed ("trojan source") without being control characters in the C0/C1 sense.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

fn push_escaped(out: &mut String, c: char) {
    let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
}

/// For a single-line context (a summary row, a log line): every control character —
/// including CR and LF — and every bidi control becomes a visible escape (`\n`, `\r`, `\t`,
/// `\u{1b}`, …), so the text can neither break onto a new line nor drive the terminal.
#[must_use]
pub fn single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || is_bidi_control(c) => push_escaped(&mut out, c),
            c => out.push(c),
        }
    }
    out
}

/// For an intentionally multi-line block: line breaks (`\n`, and `\r\n` normalized to it)
/// and tabs are kept; a lone `\r` and every other control or bidi character becomes a
/// visible escape.
#[must_use]
pub fn multi_line(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' | '\t' => out.push(c),
            '\r' => out.push_str("\\r"),
            c if c.is_control() || is_bidi_control(c) => push_escaped(&mut out, c),
            c => out.push(c),
        }
    }
    out
}

/// For an untrusted multi-line message — a JS exception text, a server's error message:
/// [`multi_line`], and additionally every line that would be read as a CI workflow command
/// (`::error …` / `::warning …` for GitHub Actions, legacy `##[…]`, after any indentation) has
/// its first character escaped, so it prints as text instead of forging an annotation or
/// masking a secret (R5B-TERM3). Legitimate line breaks are kept.
#[must_use]
pub fn untrusted_multi_line(text: &str) -> String {
    let safe = multi_line(text);
    let mut out = String::with_capacity(safe.len());
    for (i, line) in safe.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let body = line.trim_start();
        if body.starts_with("::") || body.starts_with("##[") {
            let indent = &line[..line.len() - body.len()];
            let mut chars = body.chars();
            let first = chars.next().unwrap_or(':');
            out.push_str(indent);
            push_escaped(&mut out, first);
            out.push_str(chars.as_str());
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_multi_line_defuses_workflow_command_lines_only() {
        assert_eq!(
            untrusted_multi_line("ok\n::error::x\n\t  ::set-output\n##[group]g\na::b"),
            "ok\n\\u{3a}:error::x\n\t  \\u{3a}:set-output\n\\u{23}#[group]g\na::b"
        );
        assert_eq!(untrusted_multi_line("a\r\nb\u{1b}"), "a\nb\\u{1b}");
    }

    #[test]
    fn single_line_cannot_start_a_forged_ci_annotation() {
        let hostile = "boom\n::error file=x.rs::forged\r\n::warning::also";
        let safe = single_line(hostile);
        assert!(!safe.contains('\n') && !safe.contains('\r'), "{safe}");
        assert_eq!(
            safe,
            "boom\\n::error file=x.rs::forged\\r\\n::warning::also"
        );
    }

    #[test]
    fn ansi_escapes_and_c1_controls_are_rendered_visibly() {
        let safe = single_line("red\u{1b}[31mX\u{1b}[2J\u{7f}\u{9b}");
        assert!(!safe.chars().any(char::is_control), "{safe:?}");
        assert_eq!(safe, "red\\u{1b}[31mX\\u{1b}[2J\\u{7f}\\u{9b}");
    }

    #[test]
    fn bidi_overrides_are_escaped() {
        let safe = single_line("admin\u{202E}txt.exe");
        assert_eq!(safe, "admin\\u{202e}txt.exe");
    }

    #[test]
    fn plain_text_is_unchanged() {
        let text = "com.example.app (port 7374) — ok, ünïcödé";
        assert_eq!(single_line(text), text);
        assert_eq!(multi_line(text), text);
    }

    #[test]
    fn multi_line_keeps_line_breaks_only() {
        assert_eq!(multi_line("a\r\nb\n\tc\rd\u{1b}e"), "a\nb\n\tc\\rd\\u{1b}e");
    }
}
