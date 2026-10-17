//! Comment- and format-preserving insertion of one server entry into a JSON / JSONC config.
//!
//! `victauri setup` writes into config files other tools own (`.cursor/mcp.json`,
//! `.vscode/mcp.json`, `.gemini/settings.json`, Zed's `settings.json`, …). Several of them are
//! JSONC (comments, trailing commas), and all of them belong to the user. Parsing and
//! re-serializing would drop comments and reorder keys, so instead the new entry is inserted as
//! *text* right after the opening brace of the servers object, and the result is re-parsed and
//! checked to equal "the original document plus exactly that one entry" before anything is
//! written. Anything unexpected is refused, never guessed at.

use serde_json::Value;

/// What [`insert_server`] decided.
#[derive(Debug, PartialEq, Eq)]
pub enum JsonMerge {
    /// The new file contents to write (the document plus the one new entry).
    Write(String),
    /// A server with this name is already configured — left exactly as it is.
    AlreadyPresent,
    /// Not touched, with the reason (malformed file, unexpected shape, …).
    Refused(String),
}

/// Replace every comment and trailing comma in `text` with spaces, byte for byte, so the result
/// is strict JSON with exactly the same byte offsets as the original. Newlines inside comments
/// are kept, so line structure is unchanged too. String contents are never touched.
#[must_use]
pub fn blank_jsonc(text: &str) -> String {
    let src = text.as_bytes();
    let mut out = src.to_vec();
    let mut i = 0;
    let mut in_string = false;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for b in &mut out[from..to] {
            if *b != b'\n' && *b != b'\r' {
                *b = b' ';
            }
        }
    };
    while i < src.len() {
        let c = src[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (c, src.get(i + 1)) {
            (b'"', _) => {
                in_string = true;
                i += 1;
            }
            (b'/', Some(b'/')) => {
                let start = i;
                while i < src.len() && src[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            (b'/', Some(b'*')) => {
                let start = i;
                i += 2;
                while i < src.len() && !(src[i] == b'*' && src.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(src.len());
                blank(&mut out, start, i);
            }
            _ => i += 1,
        }
    }
    // Second pass on the comment-free bytes: a comma whose next significant byte closes a
    // container is a trailing comma.
    let mut in_string = false;
    let mut i = 0;
    while i < out.len() {
        let c = out[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
        } else if c == b'"' {
            in_string = true;
        } else if c == b',' {
            let next = out[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
            if matches!(next, Some(b'}' | b']')) {
                out[i] = b' ';
            }
        }
        i += 1;
    }
    // Only ASCII bytes outside strings were replaced by ASCII spaces, so this is valid UTF-8.
    String::from_utf8(out).unwrap_or_default()
}

/// Byte offset just past the whitespace starting at `i`.
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Byte offset just past the JSON string starting at `i` (which must be `"`).
fn skip_string(b: &[u8], mut i: usize) -> usize {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Byte offset just past the JSON value starting at `i` (strict JSON, already validated).
fn skip_value(b: &[u8], i: usize) -> usize {
    match b.get(i) {
        Some(b'"') => skip_string(b, i),
        Some(b'{' | b'[') => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => {
                        j = skip_string(b, j);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            b.len()
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace()
            {
                j += 1;
            }
            j
        }
    }
}

/// The byte offset of the value of the LAST member named `key` of the object whose `{` is at
/// `open` (the last one is the one `serde_json` — and most JSON readers — honour).
fn member_value_offset(b: &[u8], open: usize, key: &str) -> Option<usize> {
    let mut i = skip_ws(b, open + 1);
    let mut found = None;
    while i < b.len() && b[i] == b'"' {
        let end = skip_string(b, i);
        let name: Option<String> = std::str::from_utf8(&b[i..end])
            .ok()
            .and_then(|s| serde_json::from_str(s).ok());
        i = skip_ws(b, end);
        if b.get(i) != Some(&b':') {
            return None;
        }
        let value = skip_ws(b, i + 1);
        if name.as_deref() == Some(key) {
            found = Some(value);
        }
        i = skip_ws(b, skip_value(b, value));
        if b.get(i) == Some(&b',') {
            i = skip_ws(b, i + 1);
        }
    }
    found
}

/// The indentation unit an existing document uses (a tab, or the leading spaces of its first
/// indented line). Defaults to two spaces.
#[must_use]
pub fn detect_indent(text: &str) -> String {
    for line in text.lines().skip(1) {
        let ws: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if ws.is_empty() || ws.len() == line.len() {
            continue;
        }
        if ws.starts_with('\t') {
            return "\t".to_string();
        }
        return ws;
    }
    "  ".to_string()
}

/// The leading whitespace of the line containing byte `pos`.
fn line_indent(text: &str, pos: usize) -> String {
    let start = text[..pos].rfind('\n').map_or(0, |n| n + 1);
    text[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Keys written first, in this order, the way every client's docs show a server entry; any
/// other key follows alphabetically (`serde_json::Map` is sorted).
const KEY_ORDER: [&str; 4] = ["type", "command", "args", "env"];

/// `value` on one line, keys in the same order a written entry uses (for "add by hand" hints).
#[must_use]
pub fn render_inline(value: &Value) -> String {
    pretty_at(value, "", "", " ")
}

/// `value` pretty-printed with `unit` indentation, every continuation line prefixed by `base`.
/// Arrays of scalars stay on one line (`"args": ["bridge", "--app", "…"]`). Every scalar is
/// emitted through `serde_json`, so strings are always escaped.
fn pretty_at(value: &Value, unit: &str, base: &str, nl: &str) -> String {
    match value {
        Value::Array(items) if items.iter().all(|v| !v.is_array() && !v.is_object()) => {
            let parts: Vec<String> = items.iter().map(Value::to_string).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Array(items) => {
            let inner = format!("{base}{unit}");
            let parts: Vec<String> = items
                .iter()
                .map(|v| format!("{inner}{}", pretty_at(v, unit, &inner, nl)))
                .collect();
            format!("[{nl}{}{nl}{base}]", parts.join(&format!(",{nl}")))
        }
        Value::Object(map) if map.is_empty() => "{}".to_string(),
        Value::Object(map) => {
            let inner = format!("{base}{unit}");
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_key(|k| {
                KEY_ORDER
                    .iter()
                    .position(|o| o == k)
                    .unwrap_or(KEY_ORDER.len())
            });
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{inner}{}: {}",
                        Value::from(k.as_str()),
                        pretty_at(&map[k], unit, &inner, nl)
                    )
                })
                .collect();
            format!("{{{nl}{}{nl}{base}}}", parts.join(&format!(",{nl}")))
        }
        scalar => scalar.to_string(),
    }
}

/// Insert `"<name>": <entry>` into the object at top-level key `servers_key` of the JSON/JSONC
/// document `existing`, creating that object if absent. `None` / blank input creates a new
/// document. Never rewrites anything else in the file.
#[must_use]
pub fn insert_server(
    existing: Option<&str>,
    servers_key: &str,
    name: &str,
    entry: &Value,
) -> JsonMerge {
    let fresh = || {
        let doc = serde_json::json!({ servers_key: { name: entry } });
        let mut out = pretty_at(&doc, "  ", "", "\n");
        out.push('\n');
        JsonMerge::Write(out)
    };
    let Some(raw) = existing else {
        return fresh();
    };
    let (bom, text) = raw
        .strip_prefix('\u{feff}')
        .map_or(("", raw), |rest| ("\u{feff}", rest));
    if text.trim().is_empty() {
        return fresh();
    }
    let blank = blank_jsonc(text);
    let original: Value = match serde_json::from_str(&blank) {
        Ok(v) => v,
        Err(e) => {
            return JsonMerge::Refused(format!(
                "it is not valid JSON/JSONC ({e}) — left untouched"
            ));
        }
    };
    let Some(root) = original.as_object() else {
        return JsonMerge::Refused("its top level is not a JSON object — left untouched".into());
    };
    match root.get(servers_key) {
        Some(Value::Object(servers)) if servers.contains_key(name) => {
            return JsonMerge::AlreadyPresent;
        }
        Some(Value::Object(_)) | None => {}
        Some(_) => {
            return JsonMerge::Refused(format!(
                "its \"{servers_key}\" key is not an object — left untouched"
            ));
        }
    }

    let b = blank.as_bytes();
    let root_open = skip_ws(b, 0);
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let unit = detect_indent(text);
    let (open, insert_value) = match member_value_offset(b, root_open, servers_key) {
        Some(off) if b.get(off) == Some(&b'{') => (off, None),
        Some(_) => return JsonMerge::Refused("unexpected document shape — left untouched".into()),
        None => (root_open, Some(serde_json::json!({ name: entry }))),
    };
    let base = line_indent(text, open);
    let member = format!("{base}{unit}");
    let (key, value) = match &insert_value {
        Some(servers_obj) => (servers_key, servers_obj),
        None => (name, entry),
    };
    let key_json = Value::from(key).to_string();
    let value_text = pretty_at(value, &unit, &member, nl);
    let after = skip_ws(b, open + 1);
    let new_text = if b.get(after) == Some(&b'}') {
        // Empty object: replace its inside with the single member.
        format!(
            "{}{nl}{member}{key_json}: {value_text}{nl}{base}{}",
            &text[..=open],
            &text[after..]
        )
    } else {
        format!(
            "{}{nl}{member}{key_json}: {value_text},{}",
            &text[..=open],
            &text[open + 1..]
        )
    };

    // Safety net: the result must parse to exactly the original document plus the new entry.
    let mut expected = original.clone();
    if let Some(obj) = expected.as_object_mut() {
        let servers = obj
            .entry(servers_key.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(servers) = servers.as_object_mut() {
            servers.insert(name.to_string(), entry.clone());
        }
    }
    match serde_json::from_str::<Value>(&blank_jsonc(&new_text)) {
        Ok(actual) if actual == expected => JsonMerge::Write(format!("{bom}{new_text}")),
        _ => JsonMerge::Refused(
            "the merge could not be verified (unusual formatting) — left untouched".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Value {
        serde_json::json!({"command": "victauri", "args": ["bridge", "--app", "com.x.app"]})
    }

    fn written(m: JsonMerge) -> String {
        match m {
            JsonMerge::Write(s) => s,
            other => panic!("expected a write, got {other:?}"),
        }
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(&blank_jsonc(s)).unwrap()
    }

    #[test]
    fn creates_a_new_document() {
        let out = written(insert_server(None, "mcpServers", "victauri", &entry()));
        assert_eq!(parse(&out)["mcpServers"]["victauri"], entry());
        assert!(out.ends_with('\n'));
        let blank = written(insert_server(Some("  \n"), "servers", "victauri", &entry()));
        assert_eq!(parse(&blank)["servers"]["victauri"], entry());
    }

    #[test]
    fn merges_without_touching_other_servers_or_keys() {
        let src = "{\n    \"zeta\": 1,\n    \"mcpServers\": {\n        \"other\": {\"command\": \"x\"}\n    },\n    \"alpha\": [1, 2]\n}\n";
        let out = written(insert_server(Some(src), "mcpServers", "victauri", &entry()));
        let v = parse(&out);
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["victauri"], entry());
        assert_eq!(v["zeta"], 1);
        assert_eq!(v["alpha"], serde_json::json!([1, 2]));
        // Original text survives verbatim around the insertion; 4-space style is kept.
        assert!(
            out.starts_with(
                "{\n    \"zeta\": 1,\n    \"mcpServers\": {\n        \"victauri\": {\n            \
                 \"command\": \"victauri\",\n            \"args\": [\"bridge\", \"--app\", \
                 \"com.x.app\"]\n        },\n        \"other\""
            ),
            "{out}"
        );
        assert!(out.contains(
            "        \"other\": {\"command\": \"x\"}\n    },\n    \"alpha\": [1, 2]\n}\n"
        ));
    }

    #[test]
    fn idempotent_second_run_is_already_present() {
        let once = written(insert_server(
            Some("{}"),
            "mcpServers",
            "victauri",
            &entry(),
        ));
        assert_eq!(
            insert_server(Some(&once), "mcpServers", "victauri", &entry()),
            JsonMerge::AlreadyPresent
        );
        // A user-customised entry is never overwritten.
        let custom =
            r#"{"mcpServers": {"victauri": {"command": "/opt/victauri", "args": ["bridge"]}}}"#;
        assert_eq!(
            insert_server(Some(custom), "mcpServers", "victauri", &entry()),
            JsonMerge::AlreadyPresent
        );
    }

    #[test]
    fn adds_missing_servers_key_and_fills_empty_object() {
        let out = written(insert_server(
            Some("{\"theme\": \"dark\"}"),
            "context_servers",
            "victauri",
            &entry(),
        ));
        let v = parse(&out);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["context_servers"]["victauri"], entry());
        let out = written(insert_server(
            Some("{\n  \"servers\": {}\n}"),
            "servers",
            "victauri",
            &entry(),
        ));
        assert_eq!(parse(&out)["servers"]["victauri"], entry());
        assert!(
            !out.contains(",\n  }"),
            "no trailing comma into an empty object: {out}"
        );
    }

    #[test]
    fn preserves_jsonc_comments_trailing_commas_bom_and_crlf() {
        let src = "\u{feff}{\r\n\t// my servers\r\n\t\"servers\": {\r\n\t\t\"a\": { \"type\": \"stdio\", \"command\": \"a\" }, // keep me\r\n\t},\r\n\t/* inputs */ \"inputs\": [],\r\n}\r\n";
        let out = written(insert_server(Some(src), "servers", "victauri", &entry()));
        assert!(out.starts_with('\u{feff}'));
        assert!(
            out.contains("// my servers")
                && out.contains("// keep me")
                && out.contains("/* inputs */")
        );
        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "line endings stay CRLF: {out:?}"
        );
        assert!(out.contains("\t\t\"victauri\": {"), "{out}");
        let v = parse(out.trim_start_matches('\u{feff}'));
        assert_eq!(v["servers"]["a"]["command"], "a");
        assert_eq!(v["servers"]["victauri"], entry());
        assert_eq!(v["inputs"], serde_json::json!([]));
    }

    #[test]
    fn comment_markers_inside_strings_are_not_comments() {
        let src = r#"{"servers": {"web": {"url": "http://localhost:1/*x*/"}}, "note": "a // b"}"#;
        let out = written(insert_server(Some(src), "servers", "victauri", &entry()));
        let v = parse(&out);
        assert_eq!(v["servers"]["web"]["url"], "http://localhost:1/*x*/");
        assert_eq!(v["note"], "a // b");
    }

    #[test]
    fn refuses_malformed_and_wrong_shapes() {
        for bad in [
            "{\"mcpServers\": ",
            "[1, 2]",
            "{\"mcpServers\": [\"x\"]}",
            "not json",
            "{\"a\": 1} {",
        ] {
            assert!(
                matches!(
                    insert_server(Some(bad), "mcpServers", "victauri", &entry()),
                    JsonMerge::Refused(_)
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn duplicate_servers_key_targets_the_last_one() {
        let src = r#"{"servers": {"a": {}}, "servers": {"b": {}}}"#;
        let out = written(insert_server(Some(src), "servers", "victauri", &entry()));
        let v = parse(&out);
        assert_eq!(v["servers"]["b"], serde_json::json!({}));
        assert_eq!(v["servers"]["victauri"], entry());
    }

    #[test]
    fn hostile_names_and_values_stay_escaped() {
        let hostile = serde_json::json!({"command": "victauri", "args": ["bridge", "--app", "x\"}, \"evil\": {\n"]});
        let out = written(insert_server(
            Some("{}"),
            "mcpServers",
            "victauri",
            &hostile,
        ));
        let v = parse(&out);
        assert_eq!(v["mcpServers"].as_object().unwrap().len(), 1);
        assert_eq!(v["mcpServers"]["victauri"], hostile);
    }

    #[test]
    fn detects_jsonc_and_indent() {
        let blank_is_same = |s: &str| blank_jsonc(s) == s;
        assert!(!blank_is_same("{\"a\": 1, // c\n}"));
        assert!(!blank_is_same("{\"a\": [1,]}"));
        assert!(blank_is_same("{\"a\": \"//\"}"));
        assert_eq!(
            blank_jsonc("{\"a\": 1 /* é */}").len(),
            "{\"a\": 1 /* é */}".len()
        );
        assert_eq!(detect_indent("{\n\t\"a\": 1\n}"), "\t");
        assert_eq!(detect_indent("{\n    \"a\": 1\n}"), "    ");
        assert_eq!(detect_indent("{}"), "  ");
    }
}
