//! `victauri tools` / `victauri call` — every Victauri tool from a plain shell.
//!
//! The MCP surface is the agent-native path, but shell-first agents (Codex without MCP, Pi,
//! `OpenClaw`, CI scripts) work through commands. These helpers turn shell arguments into a
//! tool's JSON arguments and a tool's MCP result into JSON on stdout. The connection itself is
//! the same discovered, trust-checked, identity-pinned client every other command uses.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Map, Value};

/// Exit code: the tool ran and reported an error (`isError`), or the server rejected the call.
pub const EXIT_TOOL_ERROR: i32 = 1;
/// Exit code: bad arguments, or no app could be reached.
pub const EXIT_USAGE: i32 = 2;

/// Build a tool's arguments from, in order: `--args-file` (a JSON object, `-` = stdin), an
/// inline JSON object as the first positional argument, then `key=value` (string) /
/// `key:=json` (any JSON value) pairs. Pairs override keys from the JSON sources; a key given
/// twice as a pair is an error rather than a silent "last one wins".
pub fn build_arguments(file_json: Option<&str>, args: &[String]) -> Result<Value, String> {
    let mut obj = match file_json {
        Some(text) => parse_object(text, "--args-file")?,
        None => Map::new(),
    };

    let mut rest = args;
    if let Some(first) = args.first()
        && first.trim_start().starts_with('{')
    {
        for (k, v) in parse_object(first, "the inline JSON argument")? {
            obj.insert(k, v);
        }
        rest = &args[1..];
    }

    let mut seen_pairs: Vec<String> = Vec::new();
    for arg in rest {
        let (key, value) = parse_pair(arg)?;
        if seen_pairs.contains(&key) {
            return Err(format!("argument '{key}' is given twice"));
        }
        seen_pairs.push(key.clone());
        obj.insert(key, value);
    }
    Ok(Value::Object(obj))
}

fn parse_object(text: &str, source: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(format!("{source} must be a JSON object ({{...}})")),
        Err(e) => {
            let mut msg = format!("{source} is not valid JSON: {e}");
            // Windows PowerShell 5.1 strips the double quotes inside an argument passed to a
            // native program, so '{"a":1}' arrives as '{a:1}'. Name the workaround.
            if !text.contains('"') && text.contains(':') {
                msg.push_str(
                    "\n  (no double quotes survived — a shell such as Windows PowerShell strips \
                     them from native-program arguments; use key=value / key:=json pairs, or \
                     pipe the JSON in with --args-file -)",
                );
            }
            Err(msg)
        }
    }
}

/// `key=value` → string; `key:=json` → parsed JSON value.
fn parse_pair(arg: &str) -> Result<(String, Value), String> {
    let Some(eq) = arg.find('=') else {
        return Err(format!(
            "expected key=value, key:=json or a leading JSON object, got '{}'",
            victauri_test::terminal::single_line(arg)
        ));
    };
    let (raw_key, raw_value) = (&arg[..eq], &arg[eq + 1..]);
    let (key, is_json) = match raw_key.strip_suffix(':') {
        Some(k) => (k, true),
        None => (raw_key, false),
    };
    if key.is_empty() {
        return Err(format!(
            "missing argument name in '{}'",
            victauri_test::terminal::single_line(arg)
        ));
    }
    let value = if is_json {
        serde_json::from_str(raw_value).map_err(|e| {
            format!(
                "value of '{key}' (given with :=) is not valid JSON: {e} — use {key}=… for a \
                 plain string"
            )
        })?
    } else {
        Value::String(raw_value.to_string())
    };
    Ok((key.to_string(), value))
}

/// An image block of a tool result, decoded.
pub struct Image {
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// A tool's MCP result, reduced to what a shell caller wants.
pub struct Rendered {
    /// `true` when the tool reported failure (`isError`).
    pub is_error: bool,
    /// The text content: one block → its JSON value (or the string when it is not JSON);
    /// several → an array; none → `null`.
    pub text: Value,
    /// Decoded image blocks, in order.
    pub images: Vec<Image>,
}

/// Split an MCP `CallToolResult` into its text value and decoded images.
pub fn render(result: &Value) -> Result<Rendered, String> {
    let is_error = result.get("isError").and_then(Value::as_bool) == Some(true);
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for block in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("image") => {
                let data = block.get("data").and_then(Value::as_str).unwrap_or("");
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|e| format!("the tool returned an image that is not base64: {e}"))?;
                images.push(Image {
                    mime_type: block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("application/octet-stream")
                        .to_string(),
                    bytes,
                });
            }
            _ => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    texts.push(
                        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into())),
                    );
                }
            }
        }
    }
    let text = match texts.len() {
        0 => Value::Null,
        1 => texts.remove(0),
        _ => Value::Array(texts),
    };
    Ok(Rendered {
        is_error,
        text,
        images,
    })
}

/// A tool error as one message (the text blocks, joined).
pub fn error_message(text: &Value) -> String {
    match text {
        Value::String(s) => s.clone(),
        Value::Null => "the tool reported an error with no message".to_string(),
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Where image `index` (0-based) of `count` is written for `--out <path>`: the path itself
/// for a single image, else `<stem>-<n>.<ext>` beside it.
pub fn image_path(out: &Path, index: usize, count: usize, mime_type: &str) -> PathBuf {
    if count == 1 {
        return out.to_path_buf();
    }
    let stem = out
        .file_stem()
        .map_or_else(|| "image".to_string(), |s| s.to_string_lossy().into_owned());
    let ext = out.extension().map_or_else(
        || extension_for(mime_type).to_string(),
        |e| e.to_string_lossy().into_owned(),
    );
    out.with_file_name(format!("{stem}-{}.{ext}", index + 1))
}

fn extension_for(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

/// The JSON printed for a successful call: the text value alone, or — when the result carries
/// images — `{ "text": …, "images": [{ mimeType, bytes, saved? }] }`. Image data never goes to
/// stdout (a screenshot is megabytes of base64 an agent would have to read); `saved` names the
/// file `--out` wrote.
pub fn success_json(text: Value, images: &[Image], saved: &[PathBuf]) -> Value {
    if images.is_empty() {
        return text;
    }
    let images: Vec<Value> = images
        .iter()
        .enumerate()
        .map(|(i, img)| {
            let mut entry = serde_json::json!({
                "mimeType": img.mime_type,
                "bytes": img.bytes.len(),
            });
            if let Some(path) = saved.get(i) {
                entry["saved"] = Value::String(path.display().to_string());
            }
            entry
        })
        .collect();
    serde_json::json!({ "text": text, "images": images })
}

/// One line per tool: name and the first sentence of its description.
pub fn summary_line(tool: &Value) -> Option<String> {
    let name = tool.get("name")?.as_str()?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(format!("{name:<22} {}", first_sentence(description)))
}

fn first_sentence(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    // A sentence ends at a ". " outside brackets that does not close an abbreviation —
    // "Positional bind parameters (e.g. `[1, \"x\"]`)" is one sentence, not "… (e.g.".
    let mut depth = 0_i32;
    let mut end = line.len();
    for (i, c) in line.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            '.' if depth <= 0
                && line[i + 1..].starts_with(' ')
                && !ends_with_abbreviation(&line[..i]) =>
            {
                end = i + 1;
                break;
            }
            _ => {}
        }
    }
    let sentence = &line[..end];
    // Cut on a char boundary — byte slicing would panic inside a multi-byte character.
    let short = sentence
        .char_indices()
        .nth(110)
        .map_or(sentence, |(i, _)| &sentence[..i]);
    if short.len() < sentence.len() {
        format!("{short}…")
    } else {
        short.to_string()
    }
}

fn ends_with_abbreviation(before_dot: &str) -> bool {
    let word = before_dot
        .rsplit(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_lowercase();
    matches!(word.as_str(), "e.g" | "i.e" | "etc" | "vs" | "approx")
}

/// The arguments of a tool's input schema, one line each: `name  type  [required]  description`.
/// `$ref`s into `$defs` / `definitions` are resolved one level so an action enum shows its
/// values.
pub fn schema_lines(schema: &Value) -> Vec<String> {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    props
        .iter()
        .map(|(name, prop)| {
            let prop = resolve_ref(schema, prop);
            let kind = type_label(schema, prop);
            let flag = if required.contains(&name.as_str()) {
                "  [required]"
            } else {
                ""
            };
            let description = prop
                .get("description")
                .and_then(Value::as_str)
                .map(first_sentence)
                .unwrap_or_default();
            format!("  {name:<20} {kind}{flag}  {description}")
                .trim_end()
                .to_string()
        })
        .collect()
}

fn resolve_ref<'a>(root: &'a Value, prop: &'a Value) -> &'a Value {
    let Some(reference) = prop.get("$ref").and_then(Value::as_str) else {
        return prop;
    };
    let name = reference.rsplit('/').next().unwrap_or("");
    ["$defs", "definitions"]
        .iter()
        .find_map(|defs| root.get(*defs).and_then(|d| d.get(name)))
        .unwrap_or(prop)
}

fn type_label(root: &Value, prop: &Value) -> String {
    if let Some(values) = enum_values(root, prop) {
        return format!("one of: {}", values.join(" | "));
    }
    match prop.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect::<Vec<_>>()
            .join("|"),
        _ => "any".to_string(),
    }
}

/// The allowed values of an enum-like schema: `enum`, or a `oneOf`/`anyOf` of `const`s /
/// single-value enums (how schemars renders documented enum variants).
fn enum_values(root: &Value, prop: &Value) -> Option<Vec<String>> {
    if let Some(values) = prop.get("enum").and_then(Value::as_array) {
        return Some(values.iter().map(display_value).collect());
    }
    for key in ["oneOf", "anyOf"] {
        let Some(variants) = prop.get(key).and_then(Value::as_array) else {
            continue;
        };
        let mut out = Vec::new();
        for variant in variants {
            let variant = resolve_ref(root, variant);
            if variant.get("type").and_then(Value::as_str) == Some("null") {
                continue;
            }
            if let Some(c) = variant.get("const") {
                out.push(display_value(c));
            } else if let Some(values) = variant.get("enum").and_then(Value::as_array) {
                out.extend(values.iter().map(display_value));
            } else {
                // A variant that is not a fixed value: the schema is not an enum.
                out.extend(enum_values(root, variant)?);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

fn display_value(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// Names from `known` that look like a mistyped `name` (shared prefix or containment).
pub fn suggestions<'a>(name: &str, known: &[&'a str]) -> Vec<&'a str> {
    let lower = name.to_lowercase();
    let stem: String = lower.chars().take(4).collect();
    known
        .iter()
        .copied()
        .filter(|k| {
            let k = k.to_lowercase();
            k.contains(&lower) || lower.contains(&k) || (stem.len() >= 3 && k.starts_with(&stem))
        })
        .take(5)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn pairs_are_strings_and_colon_equals_is_json() {
        let v = build_arguments(
            None,
            &args(&[
                "query=SELECT 1",
                "limit:=5",
                "deep:={\"a\":[1,true]}",
                "flag:=true",
            ]),
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"query": "SELECT 1", "limit": 5, "deep": {"a": [1, true]}, "flag": true})
        );
    }

    #[test]
    fn a_value_may_contain_equals_signs_and_be_empty() {
        let v = build_arguments(None, &args(&["code=a == b ? 1 : 2", "label="])).unwrap();
        assert_eq!(v, json!({"code": "a == b ? 1 : 2", "label": ""}));
    }

    #[test]
    fn a_number_given_with_equals_stays_a_string() {
        // `id=123` must reach the tool as the string "123" — type coercion is opt-in (:=).
        let v = build_arguments(None, &args(&["id=123"])).unwrap();
        assert_eq!(v, json!({"id": "123"}));
    }

    #[test]
    fn inline_json_then_pairs_override() {
        let v =
            build_arguments(None, &args(&[r#"{"action":"list","limit":1}"#, "limit:=9"])).unwrap();
        assert_eq!(v, json!({"action": "list", "limit": 9}));
    }

    #[test]
    fn file_json_is_the_base_layer() {
        let v = build_arguments(
            Some(r#"{"query":"from file","max_rows":3}"#),
            &args(&["max_rows:=1"]),
        )
        .unwrap();
        assert_eq!(v, json!({"query": "from file", "max_rows": 1}));
    }

    #[test]
    fn no_arguments_is_an_empty_object() {
        assert_eq!(build_arguments(None, &[]).unwrap(), json!({}));
    }

    #[test]
    fn malformed_arguments_are_rejected_with_the_reason() {
        assert!(
            build_arguments(None, &args(&["noequals"]))
                .unwrap_err()
                .contains("key=value")
        );
        assert!(
            build_arguments(None, &args(&["=x"]))
                .unwrap_err()
                .contains("missing argument name")
        );
        assert!(
            build_arguments(None, &args(&["n:=nope"]))
                .unwrap_err()
                .contains("not valid JSON")
        );
        assert!(
            build_arguments(None, &args(&["a=1", "a=2"]))
                .unwrap_err()
                .contains("twice")
        );
        assert!(
            build_arguments(None, &args(&["[1,2]"]))
                .unwrap_err()
                .contains("key=value"),
            "a JSON array is not a JSON object"
        );
        assert!(
            build_arguments(Some("[1]"), &[])
                .unwrap_err()
                .contains("must be a JSON object")
        );
    }

    #[test]
    fn quote_stripped_json_names_the_powershell_workaround() {
        // What Windows PowerShell 5.1 delivers for '{"action":"list"}'.
        let err = build_arguments(None, &args(&["{action:list}"])).unwrap_err();
        assert!(err.contains("--args-file -"), "{err}");
    }

    #[test]
    fn a_hostile_argument_is_echoed_on_one_line() {
        let err = build_arguments(None, &args(&["bad\n::error ::forged"])).unwrap_err();
        assert!(!err.contains('\n'), "{err}");
    }

    #[test]
    fn render_parses_a_single_json_text_block() {
        let r = render(&json!({"content": [{"type": "text", "text": "{\"rows\":[1]}"}]})).unwrap();
        assert!(!r.is_error);
        assert_eq!(r.text, json!({"rows": [1]}));
        assert!(r.images.is_empty());
    }

    #[test]
    fn render_keeps_plain_text_as_a_string_and_flags_errors() {
        let r = render(&json!({"isError": true,
                               "content": [{"type": "text", "text": "selector not found"}]}))
        .unwrap();
        assert!(r.is_error);
        assert_eq!(error_message(&r.text), "selector not found");
    }

    #[test]
    fn render_decodes_images_and_never_echoes_their_data() {
        let png = [0x89, b'P', b'N', b'G', 1, 2, 3];
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let r = render(&json!({"content": [
            {"type": "text", "text": "{\"points\":2}"},
            {"type": "image", "data": b64, "mimeType": "image/png"},
            {"type": "image", "data": b64, "mimeType": "image/png"},
        ]}))
        .unwrap();
        assert_eq!(r.images.len(), 2);
        assert_eq!(r.images[0].bytes, png);
        let out = success_json(r.text, &r.images, &[PathBuf::from("a-1.png")]);
        assert_eq!(out["text"], json!({"points": 2}));
        assert_eq!(out["images"][0]["bytes"], 7);
        assert_eq!(out["images"][0]["saved"], "a-1.png");
        assert!(out["images"][1].get("saved").is_none());
        assert!(!out.to_string().contains(&b64), "no base64 on stdout");
    }

    #[test]
    fn render_rejects_an_image_that_is_not_base64() {
        assert!(
            render(
                &json!({"content": [{"type": "image", "data": "%%%", "mimeType": "image/png"}]})
            )
            .is_err()
        );
    }

    #[test]
    fn several_text_blocks_become_an_array() {
        let r = render(&json!({"content": [
            {"type": "text", "text": "1"}, {"type": "text", "text": "two"}
        ]}))
        .unwrap();
        assert_eq!(r.text, json!([1, "two"]));
        assert_eq!(render(&json!({})).unwrap().text, Value::Null);
    }

    #[test]
    fn image_paths_number_only_when_there_are_several() {
        let out = Path::new("shots/frame.png");
        assert_eq!(
            image_path(out, 0, 1, "image/png"),
            PathBuf::from("shots/frame.png")
        );
        assert_eq!(
            image_path(out, 1, 3, "image/png"),
            PathBuf::from("shots/frame-2.png")
        );
        assert_eq!(
            image_path(Path::new("frames"), 0, 2, "image/png"),
            PathBuf::from("frames-1.png")
        );
    }

    #[test]
    fn schema_lines_show_types_requirement_and_enum_values() {
        let schema = json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"$ref": "#/$defs/Action", "description": "What to do. More text."},
                "limit": {"type": ["integer", "null"], "description": "Max rows"},
                "mode": {"oneOf": [{"const": "fast"}, {"const": "slow"}]},
            },
            "$defs": {"Action": {"type": "string", "enum": ["list", "clear"],
                                 "description": "What to do. More text."}},
        });
        let lines = schema_lines(&schema).join("\n");
        assert!(
            lines.contains("action") && lines.contains("one of: list | clear"),
            "{lines}"
        );
        assert!(lines.contains("[required]  What to do."), "{lines}");
        assert!(
            !lines.contains("More text"),
            "first sentence only:\n{lines}"
        );
        assert!(
            lines.contains("limit") && lines.contains("integer"),
            "{lines}"
        );
        assert!(lines.contains("one of: fast | slow"), "{lines}");
        assert!(schema_lines(&json!({"type": "object"})).is_empty());
    }

    #[test]
    fn summary_takes_the_first_sentence() {
        let line = summary_line(&json!({
            "name": "query_db",
            "description": "Run a read-only SQL query. Uses an authorizer.\nMore."
        }))
        .unwrap();
        assert!(line.starts_with("query_db"));
        assert!(line.ends_with("Run a read-only SQL query."), "{line}");
    }

    #[test]
    fn a_sentence_does_not_end_inside_brackets_or_after_an_abbreviation() {
        // Live `query_db` param description that used to print as "… (e.g.".
        assert_eq!(
            first_sentence("Positional bind parameters for the query (e.g. `[1, \"x\"]`). More."),
            "Positional bind parameters for the query (e.g. `[1, \"x\"]`)."
        );
        assert_eq!(
            first_sentence("Ref handle, e.g. e12. Get it from a snapshot."),
            "Ref handle, e.g. e12."
        );
        assert_eq!(
            first_sentence("Unbalanced ) bracket. Next."),
            "Unbalanced ) bracket."
        );
    }

    #[test]
    fn suggestions_find_near_names() {
        let known = ["query_db", "dom_snapshot", "invoke_command", "introspect"];
        assert_eq!(suggestions("query", &known), vec!["query_db"]);
        assert_eq!(suggestions("invoke", &known), vec!["invoke_command"]);
        assert!(suggestions("zzz", &known).is_empty());
    }
}
