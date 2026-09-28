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

#[cfg(test)]
mod tests {
    use super::*;

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
