//! Parsing JSON produced by page JavaScript.
//!
//! `JSON.stringify` in the webview escapes an unpaired UTF-16 surrogate as `\ud800`-style text
//! (well-formed stringify, ES2019). That is valid JSON to JavaScript but not to `serde_json`,
//! which rejects it — so one truncated emoji, or one hostile `console.log('\ud800')`, made the
//! whole result unparseable: the recording drain stalled, and `explain` / `introspect` silently
//! came back empty. Every Rust parse of page-produced JSON goes through
//! [`sanitize_lone_surrogates`] (or [`parse_page_json`]) first.

use std::borrow::Cow;

/// Rewrite every `\uXXXX` escape of an unpaired surrogate (U+D800–U+DFFF not part of a valid
/// high+low escape pair) to `�`, the replacement character. Escape-aware: `\\ud800` is an
/// escaped backslash followed by text, not an escape, and is left alone. Valid pairs are kept.
/// Returns the input unchanged (borrowed) when there is nothing to rewrite.
#[must_use]
pub fn sanitize_lone_surrogates(json: &str) -> Cow<'_, str> {
    let bytes = json.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        let Some(cp) = unicode_escape_at(bytes, i) else {
            // Any other escape (`\\`, `\"`, `\n`, …) is two bytes; skipping both keeps an
            // escaped backslash from being mistaken for the start of a `\u` escape.
            i += 2;
            continue;
        };
        let lone = match cp {
            0xD800..=0xDBFF => {
                if unicode_escape_at(bytes, i + 6).is_some_and(|lo| (0xDC00..=0xDFFF).contains(&lo))
                {
                    i += 12; // a valid pair
                    continue;
                }
                true
            }
            0xDC00..=0xDFFF => true,
            _ => false,
        };
        if lone {
            let buf = out.get_or_insert_with(|| String::with_capacity(json.len()));
            buf.push_str(&json[copied..i]);
            buf.push_str("\\ufffd");
            copied = i + 6;
        }
        i += 6;
    }
    match out {
        None => Cow::Borrowed(json),
        Some(mut buf) => {
            buf.push_str(&json[copied..]);
            Cow::Owned(buf)
        }
    }
}

/// The code unit of a `\uXXXX` escape starting at byte `at`, if there is one.
fn unicode_escape_at(bytes: &[u8], at: usize) -> Option<u32> {
    let esc = bytes.get(at..at + 6)?;
    if esc[0] != b'\\' || esc[1] != b'u' {
        return None;
    }
    let hex = std::str::from_utf8(&esc[2..]).ok()?;
    u32::from_str_radix(hex, 16).ok()
}

/// `serde_json::from_str` for JSON produced by page JavaScript (see the module docs).
///
/// # Errors
///
/// Returns the `serde_json` error if the sanitized text still does not parse as `T`.
pub fn parse_page_json<T: serde::de::DeserializeOwned>(json: &str) -> serde_json::Result<T> {
    serde_json::from_str(&sanitize_lone_surrogates(json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_surrogates_become_replacement_characters() {
        let raw = r#"["a\ud800b","\udc00","x\uD83D"]"#;
        assert!(serde_json::from_str::<Vec<String>>(raw).is_err());
        let v: Vec<String> = parse_page_json(raw).unwrap();
        assert_eq!(v, vec!["a\u{FFFD}b", "\u{FFFD}", "x\u{FFFD}"]);
    }

    #[test]
    fn valid_pairs_and_escaped_backslashes_are_untouched() {
        let pair = r#""😀""#;
        assert!(matches!(sanitize_lone_surrogates(pair), Cow::Borrowed(_)));
        assert_eq!(parse_page_json::<String>(pair).unwrap(), "\u{1F600}");
        // `\\ud800` is a literal backslash followed by "ud800".
        let text = r#""\\ud800""#;
        assert!(matches!(sanitize_lone_surrogates(text), Cow::Borrowed(_)));
        assert_eq!(parse_page_json::<String>(text).unwrap(), "\\ud800");
        // A high surrogate followed by a non-low escape is still lone.
        let v: String = parse_page_json(r#""\ud800A""#).unwrap();
        assert_eq!(v, "\u{FFFD}A");
    }

    #[test]
    fn truncated_input_does_not_panic() {
        for raw in [r"\", r"\u", r"\ud8", r#""\ud800"#, r"\ud800\u"] {
            let _ = sanitize_lone_surrogates(raw);
        }
        // Multi-byte text around escapes keeps slicing on char boundaries.
        let v: String = parse_page_json(r#""é\ud800ü""#).unwrap();
        assert_eq!(v, "é\u{FFFD}ü");
    }
}
