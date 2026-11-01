use regex::RegexSet;

const BUILTIN_PATTERNS: &[&str] = &[
    // API keys: sk-..., pk-..., key-... (20+ chars)
    r"(?i)\b(sk|pk|key)[-_][a-zA-Z0-9]{20,}\b",
    // Bearer tokens in output
    r"(?i)bearer\s+[a-zA-Z0-9\-_.~+/]{20,}",
    // AWS keys
    r"\bAKIA[0-9A-Z]{16}\b",
    // JWT tokens (3 base64 sections separated by dots)
    r"\beyJ[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}\b",
    // Generic long hex secrets (40+ hex chars — SHA1 hashes, API keys)
    r"\b[0-9a-fA-F]{40,}\b",
    // Email addresses
    r"\b[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}\b",
    // Credit card numbers (basic patterns)
    r"\b\d{4}[- ]?\d{4}[- ]?\d{4}[- ]?\d{4}\b",
    // OpenAI-style keys
    r"\bsk-[a-zA-Z0-9]{32,}\b",
    // Anthropic keys
    r"\bsk-ant-[a-zA-Z0-9\-]{20,}\b",
    // GitHub tokens
    r"\b(ghp|gho|ghu|ghs|ghr)_[a-zA-Z0-9]{36,}\b",
    // Stripe keys
    r"\b(sk|pk|rk)_(test|live)_[a-zA-Z0-9]{20,}\b",
];

const SENSITIVE_JSON_KEYS: &[&str] = &[
    "api_key",
    "apiKey",
    "api-key",
    "secret",
    "password",
    "passwd",
    "token",
    "access_token",
    "refresh_token",
    "private_key",
    "privateKey",
    "secret_key",
    "secretKey",
    "authorization",
    "auth_token",
    "session_token",
    "cookie",
    "credentials",
    "ssn",
    "credit_card",
    "card_number",
    "passphrase",
    "jwt",
];

/// Credential names too short to match as a substring (`auth` is in `author` and `oauth_provider`):
/// these count only as a whole `_` / `-` / `.` separated part of a key.
const SENSITIVE_KEY_PARTS: &[&str] = &["auth", "pwd"];

/// URL query/fragment parameters whose value is a credential. Matched only as a whole parameter
/// name right after `?`, `&` or `#`, so `?keyboard=` or `&tokens_used=` are left alone.
const SECRET_URL_PARAMS: &[&str] = &[
    "access_token",
    "access-token",
    "refresh_token",
    "id_token",
    "auth_token",
    "token",
    "api_key",
    "api-key",
    "apikey",
    "key",
    "client_secret",
    "secret",
    "password",
    "passwd",
    "pwd",
    "auth",
    "sig",
    "signature",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "x-goog-signature",
    "x-goog-credential",
];

/// `?name=value` / `&name=value` / `#name=value` for a [`SECRET_URL_PARAMS`] name; the value runs
/// to the next separator, whitespace, quote or backslash (the end of a JSON string).
static URL_SECRET_PARAM_RE: std::sync::LazyLock<Option<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        let names: Vec<String> = SECRET_URL_PARAMS.iter().map(|n| regex::escape(n)).collect();
        compile_builtin(&format!(
            r#"(?i)([?&#](?:{})=)[^&#\s"'<>\\]+"#,
            names.join("|")
        ))
    });

/// `"<key containing a sensitive word>": "<string>"` or `: <number>` in free text — JSON
/// embedded in a non-JSON output, a truncated JSON fragment, or the content of a JSON string. A
/// string value cut off by the end of the text (a truncated body) is redacted to the end.
static TEXT_KEY_VALUE_RE: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(
    || {
        let mut words: Vec<String> = SENSITIVE_JSON_KEYS
            .iter()
            .map(|k| regex::escape(&k.to_lowercase()))
            .collect();
        words.sort();
        words.dedup();
        compile_builtin(&format!(
            r#"(?i)("(?:[^"\\\n]{{0,64}}?(?:{})[^"\\\n]{{0,64}}?|(?:[^"\\\n]{{0,64}}?[_.-])?(?:{})(?:[_.-][^"\\\n]{{0,64}}?)?)"\s*:\s*)("(?:[^"\\]|\\.)*(?:"|\z)|-?\d[\d.eE+-]*)"#,
            words.join("|"),
            SENSITIVE_KEY_PARTS.join("|")
        ))
    },
);

/// `<name ending in a sensitive word>=<value>` in free text — the shape of `tracing` span fields
/// (`login{password="…" api_key=…}`, which `#[instrument]` records for every argument), log lines
/// like `token=abc123`, and `key=value` config dumps. The name is the sensitive word, optionally
/// after `prefix_` / `prefix-` / `prefix.` components (`db_password`, `x-api_key`) — a word that
/// merely starts the name (`tokens_used`, `token_count`) is not a credential. The value is a
/// quoted string (to its closing quote, or the end of a truncated text) or a bare word.
static TEXT_KEY_EQ_RE: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
    let mut words: Vec<String> = SENSITIVE_JSON_KEYS
        .iter()
        .map(|k| regex::escape(&k.to_lowercase()))
        .collect();
    words.extend(SENSITIVE_KEY_PARTS.iter().map(|k| (*k).to_string()));
    words.sort();
    words.dedup();
    compile_builtin(&format!(
        r#"(?i)(\b(?:[a-z0-9]{{1,32}}[_.-]){{0,4}}(?:{})\s*=\s*)("(?:[^"\\]|\\.)*(?:"|\z)|'[^'\n]*(?:'|\z)|[^\s,;&}})\]"'\[][^\s,;&}})\]"']*)"#,
        words.join("|")
    ))
});

/// Compile one of the built-in patterns; a failure is a bug, logged instead of panicking.
fn compile_builtin(pattern: &str) -> Option<regex::Regex> {
    regex::Regex::new(pattern)
        .inspect_err(|e| tracing::error!("BUG: built-in redaction pattern failed to compile: {e}"))
        .ok()
}

/// Replace the value of every sensitive `"key": value` and `key=value` pair found in free text.
fn redact_text_key_values(text: &str) -> std::borrow::Cow<'_, str> {
    let colon = match TEXT_KEY_VALUE_RE.as_ref() {
        Some(re) => re.replace_all(text, r#"${1}"[REDACTED]""#),
        None => std::borrow::Cow::Borrowed(text),
    };
    match TEXT_KEY_EQ_RE.as_ref() {
        Some(re) if re.is_match(&colon) => {
            std::borrow::Cow::Owned(re.replace_all(&colon, "${1}[REDACTED]").into_owned())
        }
        _ => colon,
    }
}

/// Output redactor that scrubs API keys, tokens, emails, and sensitive JSON keys
/// from MCP tool output. Applies built-in patterns plus optional custom regexes.
pub struct Redactor {
    builtin_set: RegexSet,
    builtin_compiled: Vec<regex::Regex>,
    custom_set: Option<RegexSet>,
    custom_compiled: Vec<regex::Regex>,
}

impl Redactor {
    /// Build a redactor with custom patterns.
    ///
    /// # Errors
    ///
    /// Returns [`regex::Error`] if any custom pattern (or a built-in pattern) fails to compile.
    pub fn try_new(custom_patterns: &[String]) -> Result<Self, regex::Error> {
        let builtin_set = RegexSet::new(BUILTIN_PATTERNS)?;
        let builtin_compiled: Vec<regex::Regex> = BUILTIN_PATTERNS
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();

        let (custom_set, custom_compiled) = if custom_patterns.is_empty() {
            (None, Vec::new())
        } else {
            let set = RegexSet::new(custom_patterns)?;
            let compiled: Vec<regex::Regex> = custom_patterns
                .iter()
                .map(|p| regex::Regex::new(p))
                .collect::<Result<Vec<_>, _>>()?;
            (Some(set), compiled)
        };

        Ok(Self {
            builtin_set,
            builtin_compiled,
            custom_set,
            custom_compiled,
        })
    }

    /// Build a redactor with custom patterns, logging a warning and skipping any invalid patterns.
    ///
    /// If the built-in redaction patterns fail to compile (a bug), falls back to
    /// an empty set and logs an error rather than panicking.
    pub fn new(custom_patterns: &[String]) -> Self {
        let (builtin_set, builtin_compiled) = match RegexSet::new(BUILTIN_PATTERNS) {
            Ok(set) => {
                let compiled: Vec<regex::Regex> = BUILTIN_PATTERNS
                    .iter()
                    .filter_map(|p| regex::Regex::new(p).ok())
                    .collect();
                (set, compiled)
            }
            Err(e) => {
                tracing::error!(
                    "BUG: built-in redaction patterns failed to compile: {e}. \
                     Redaction will be disabled."
                );
                // Fall back to an empty set so the process survives.
                // An empty RegexSet always compiles successfully.
                let empty: Vec<String> = Vec::new();
                let empty_set = RegexSet::new(&empty).unwrap_or_else(|_| unreachable!());
                (empty_set, Vec::new())
            }
        };

        let (custom_set, custom_compiled) = if custom_patterns.is_empty() {
            (None, Vec::new())
        } else {
            // One by one: `RegexSet::new` fails as a whole, which dropped the valid ones too.
            let valid: Vec<&String> = custom_patterns
                .iter()
                .filter(|p| match regex::Regex::new(p) {
                    Ok(_) => true,
                    Err(e) => {
                        tracing::warn!("Skipping invalid custom redaction pattern: {e}");
                        false
                    }
                })
                .collect();
            match RegexSet::new(&valid) {
                Ok(set) => {
                    let compiled: Vec<regex::Regex> = valid
                        .iter()
                        .filter_map(|p| regex::Regex::new(p).ok())
                        .collect();
                    (Some(set), compiled)
                }
                Err(e) => {
                    tracing::warn!("Failed to compile custom redaction patterns: {e}");
                    (None, Vec::new())
                }
            }
        };

        Self {
            builtin_set,
            builtin_compiled,
            custom_set,
            custom_compiled,
        }
    }

    /// Scrub sensitive data from `input` using regex patterns and JSON-key matching.
    ///
    /// Output that is JSON is walked as JSON, including JSON carried inside its string values
    /// (IPC/network bodies are JSON-encoded strings): sensitive keys lose their value, and the
    /// value patterns (API keys, bearer tokens, JWTs, emails, … plus any custom patterns) and
    /// credential URL parameters (`?access_token=…`, `&sig=…`) apply to every leaf and key. The
    /// patterns must not run over the raw JSON text first: a card-shaped number would become a
    /// bare `[REDACTED]`, the text would stop being JSON, and key redaction would be lost with it.
    /// Other output gets the patterns, then `"key": value` pairs found in the text. Best effort:
    /// see the limits in `docs/src/security.md`.
    #[must_use]
    pub fn redact(&self, input: &str) -> String {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
            return self.redact_text(input);
        };
        let output = match self.redact_json_value(&value, 0) {
            Some(redacted) => {
                serde_json::to_string(&redacted).unwrap_or_else(|_| self.redact_text(input))
            }
            None => input.to_string(),
        };
        // A custom pattern may be written against the JSON text itself (`"internal":\s*"\w+"`).
        // Leaves cannot match that, and a redacted result beats a valid one.
        match &self.custom_set {
            Some(set) if set.is_match(&output) => {
                let mut output = output;
                for re in &self.custom_compiled {
                    output = re.replace_all(&output, "[REDACTED]").into_owned();
                }
                output
            }
            _ => output,
        }
    }

    /// Value patterns, URL parameters and `key: value` pairs over text that is not JSON.
    fn redact_text(&self, input: &str) -> String {
        let mut output = self.redact_regex(input);
        if let Some(re) = URL_SECRET_PARAM_RE.as_ref()
            && re.is_match(&output)
        {
            output = re.replace_all(&output, "${1}[REDACTED]").into_owned();
        }
        redact_text_key_values(&output).into_owned()
    }

    fn redact_regex(&self, input: &str) -> String {
        let has_builtin = self.builtin_set.is_match(input);
        let has_custom = self.custom_set.as_ref().is_some_and(|c| c.is_match(input));

        if !has_builtin && !has_custom {
            return input.to_string();
        }

        let mut output = input.to_string();

        if has_builtin {
            for re in &self.builtin_compiled {
                output = re.replace_all(&output, "[REDACTED]").to_string();
            }
        }

        if has_custom {
            for re in &self.custom_compiled {
                output = re.replace_all(&output, "[REDACTED]").to_string();
            }
        }

        output
    }

    /// `value` with every sensitive key's value redacted and the patterns applied to its leaves
    /// and keys, or `None` if nothing needed redacting. A string that holds JSON is redacted
    /// inside (and re-encoded); any other string goes through [`Self::redact_text`] (which
    /// covers a truncated JSON body, say).
    fn redact_json_value(
        &self,
        value: &serde_json::Value,
        depth: usize,
    ) -> Option<serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => {
                // `{name|key: <sensitive>, value: …}` (cookies, key/value lists): the sensitive
                // word is in the sibling field.
                let named_secret = ["name", "key"].iter().any(|field| {
                    map.get(*field)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(is_sensitive_key)
                });
                let mut changed = false;
                let mut new_map = serde_json::Map::new();
                for (key, val) in map {
                    let secret = is_sensitive_key(key) || (named_secret && key == "value");
                    let new_val = if secret && !val.is_boolean() {
                        // Booleans like `has_api_key: true` only indicate presence — kept.
                        Some(serde_json::Value::String("[REDACTED]".into()))
                    } else {
                        self.redact_json_value(val, depth)
                    };
                    changed |= new_val.is_some();
                    let new_key = self.redact_regex(key);
                    changed |= new_key != *key;
                    // Keys that redact to the same text must not overwrite each other's values.
                    let mut unique = new_key.clone();
                    let mut n = 1;
                    while new_map.contains_key(&unique) {
                        n += 1;
                        unique = format!("{new_key}#{n}");
                    }
                    new_map.insert(unique, new_val.unwrap_or_else(|| val.clone()));
                }
                changed.then_some(serde_json::Value::Object(new_map))
            }
            serde_json::Value::Array(arr) => {
                let redacted: Vec<Option<serde_json::Value>> = arr
                    .iter()
                    .map(|v| self.redact_json_value(v, depth))
                    .collect();
                redacted.iter().any(Option::is_some).then(|| {
                    serde_json::Value::Array(
                        redacted
                            .into_iter()
                            .zip(arr)
                            .map(|(new, old)| new.unwrap_or_else(|| old.clone()))
                            .collect(),
                    )
                })
            }
            serde_json::Value::String(text) => self.redact_json_string(text, depth),
            // A card-shaped number is replaced by a string, which keeps the output JSON.
            serde_json::Value::Number(number) => {
                let text = number.to_string();
                (self.redact_regex(&text) != text)
                    .then(|| serde_json::Value::String("[REDACTED]".into()))
            }
            _ => None,
        }
    }

    fn redact_json_string(&self, text: &str, depth: usize) -> Option<serde_json::Value> {
        let trimmed = text.trim_start();
        if depth < MAX_NESTED_JSON_DEPTH
            && (trimmed.starts_with('{') || trimmed.starts_with('['))
            && let Ok(inner) = serde_json::from_str::<serde_json::Value>(text)
        {
            let redacted = self.redact_json_value(&inner, depth + 1)?;
            return serde_json::to_string(&redacted)
                .ok()
                .map(serde_json::Value::String);
        }
        let redacted = self.redact_text(text);
        (redacted != text).then_some(serde_json::Value::String(redacted))
    }
}

/// How deep JSON nested inside JSON strings is followed.
const MAX_NESTED_JSON_DEPTH: usize = 4;

/// Whether a JSON key or storage key names a credential.
pub(crate) fn is_sensitive_key(key: &str) -> bool {
    let lower_key = key.to_lowercase();
    SENSITIVE_JSON_KEYS
        .iter()
        .any(|k| lower_key.contains(&k.to_lowercase()))
        || lower_key
            .split(['_', '-', '.'])
            .any(|part| SENSITIVE_KEY_PARTS.contains(&part))
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_api_keys() {
        let r = Redactor::default();
        assert!(
            r.redact("key is sk-abc123def456ghi789jkl012mno")
                .contains("[REDACTED]")
        );
    }

    #[test]
    fn redacts_bearer_tokens() {
        let r = Redactor::default();
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let output = r.redact(input);
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("eyJhbGci"));
    }

    #[test]
    fn redacts_emails() {
        let r = Redactor::default();
        assert!(
            r.redact("contact user@example.com for help")
                .contains("[REDACTED]")
        );
    }

    #[test]
    fn passes_through_clean_text() {
        let r = Redactor::default();
        let input = r#"{"ok": true, "title": "My App"}"#;
        assert_eq!(r.redact(input), input);
    }

    #[test]
    fn custom_patterns_work() {
        let r = Redactor::new(&["secret_\\w+".to_string()]);
        assert!(
            r.redact("found secret_project_alpha here")
                .contains("[REDACTED]")
        );
    }

    #[test]
    fn redacts_json_sensitive_keys() {
        let r = Redactor::default();
        let input = r#"{"api_key":"sk-test-12345","name":"John","token":"abc123"}"#;
        let output = r.redact(input);
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains("John"));
        assert!(!output.contains("sk-test-12345"));
    }

    #[test]
    fn preserves_boolean_sensitive_keys() {
        let r = Redactor::default();
        let input = r#"{"has_api_key":true,"api_key":"secret-value-here"}"#;
        let output = r.redact(input);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["has_api_key"], serde_json::Value::Bool(true));
        assert_eq!(
            parsed["api_key"],
            serde_json::Value::String("[REDACTED]".into())
        );
    }

    #[test]
    fn redacts_nested_json_keys() {
        let r = Redactor::default();
        let input = r#"{"config":{"llm":{"api_key":"sk-live-xxx","model":"gpt-4"}}}"#;
        let output = r.redact(input);
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains("gpt-4"));
        assert!(!output.contains("sk-live-xxx"));
    }

    /// R5B-REDACT1: secrets in URL query strings were not matched (tokens ride in IPC/network
    /// URLs), and key-based redaction only applied when the WHOLE output parsed as JSON.
    #[test]
    fn redacts_secret_query_parameters() {
        let r = Redactor::default();
        let out = r.redact(
            "GET https://api.example.com/v1/items?user=7&access_token=abc123def&page=2              https://x.test/cb#id_token=eyJraWQ&state=s https://h.test/?API_KEY=k9k9k9              https://blob.test/f?sv=2024&sig=Zx%2Fq&se=1",
        );
        for secret in ["abc123def", "eyJraWQ", "k9k9k9", "Zx%2Fq"] {
            assert!(!out.contains(secret), "{secret} survived: {out}");
        }
        for kept in ["user=7", "page=2", "state=s", "sv=2024", "se=1"] {
            assert!(out.contains(kept), "{kept} was redacted: {out}");
        }
        // Not a secret parameter: only whole parameter names match.
        let clean = "https://x.test/?keyboard=us&monkey=1&tokens_used=5";
        assert_eq!(r.redact(clean), clean);
        // Inside a JSON string, the URL is redacted and the JSON stays valid.
        let json = r#"{"url":"https://x.test/a?token=zzz111&b=1"}"#;
        let v: serde_json::Value = serde_json::from_str(&r.redact(json)).unwrap();
        assert_eq!(v["url"], "https://x.test/a?token=[REDACTED]&b=1");
    }

    #[test]
    fn redacts_sensitive_keys_in_json_embedded_in_text() {
        let r = Redactor::default();
        let out =
            r.redact(r#"invoke login -> {"ok":true,"session_token":"s3cr3t-value","n":1} done"#);
        assert!(!out.contains("s3cr3t-value"), "{out}");
        assert!(out.contains(r#""ok":true"#), "{out}");
        // A truncated JSON fragment (log fields are cut at 4 KB) still has its keys redacted.
        let out = r.redact(r#"body: {"password": "hunter2", "data": "xxxxxxxx…(truncated"#);
        assert!(!out.contains("hunter2"), "{out}");
        // …including when the cut falls inside the secret itself.
        let out = r.redact(r#"body: {"id": 1, "secret": "abcdefgh"#);
        assert!(!out.contains("abcdefgh"), "{out}");
        assert!(out.contains(r#""id": 1"#), "{out}");
    }

    #[test]
    fn redacts_sensitive_keys_in_json_nested_inside_json_strings() {
        let r = Redactor::default();
        // IPC/network logs carry bodies as JSON-encoded strings inside the JSON result.
        let input = serde_json::json!({
            "command": "get_settings",
            "response": "{\"llm\":{\"api_key\":\"sk-live-abc\",\"model\":\"m1\"}}",
            "truncated_body": "{\"refresh_token\":\"rt-999\",\"x\":\"…"
        })
        .to_string();
        let out = r.redact(&input);
        let v: serde_json::Value = serde_json::from_str(&out).expect("still valid JSON");
        let response = v["response"].as_str().unwrap();
        assert!(!response.contains("sk-live-abc"), "{out}");
        assert!(response.contains("m1"), "{out}");
        assert!(
            !v["truncated_body"].as_str().unwrap().contains("rt-999"),
            "{out}"
        );
        assert_eq!(v["command"], "get_settings");
    }

    /// Backend log span context is `name{key=value …}` text (`#[instrument]` records every
    /// argument) — the same secret that is redacted as an event field must be redacted there.
    #[test]
    fn redacts_key_equals_value_pairs_in_text() {
        let r = Redactor::default();
        let input = serde_json::json!({
            "spans": ["login{user=\"ann\" password=\"hunter2xyz\" api_key=plainsecret}"],
            "message": "retrying with token=abc123secret after 3s",
            "fields": {"tokens_used": 5, "elapsed_ms": 40}
        })
        .to_string();
        let out = r.redact(&input);
        let v: serde_json::Value = serde_json::from_str(&out).expect("still valid JSON");
        let span = v["spans"][0].as_str().unwrap();
        assert!(
            !span.contains("hunter2xyz") && !span.contains("plainsecret"),
            "{out}"
        );
        assert!(
            span.contains("user=\"ann\""),
            "non-sensitive fields survive: {out}"
        );
        assert!(!out.contains("abc123secret"), "{out}");
        assert!(v["message"].as_str().unwrap().contains("after 3s"), "{out}");
        assert_eq!(v["fields"]["elapsed_ms"], 40);
        // Plain (non-JSON) text too, including a value cut off by truncation.
        let plain = r.redact("connect secret='s3cr3t' then password=\"unterminated");
        assert!(
            !plain.contains("s3cr3t") && !plain.contains("unterminated"),
            "{plain}"
        );
        let prefixed = r.redact("db_password=pw1 x-api_key=k2 tokens_used=5 token_count=9");
        assert!(
            !prefixed.contains("pw1") && !prefixed.contains("k2"),
            "{prefixed}"
        );
        assert!(
            prefixed.contains("tokens_used=5") && prefixed.contains("token_count=9"),
            "{prefixed}"
        );
        // Already-redacted values are left alone (no doubled markers).
        assert_eq!(r.redact("token=[REDACTED] x"), "token=[REDACTED] x");
    }

    #[test]
    fn redacts_github_tokens() {
        let r = Redactor::default();
        assert!(
            r.redact("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno")
                .contains("[REDACTED]")
        );
    }

    #[test]
    fn redacts_stripe_keys() {
        let r = Redactor::default();
        assert!(
            r.redact("sk_test_ABCDEFGHIJKLMNOPQRSTUVWXYZab")
                .contains("[REDACTED]")
        );
    }

    /// A 16-digit integer (a microsecond timestamp) matches the card pattern. Run over the raw
    /// text, that wrote a bare `[REDACTED]` into the JSON: the output no longer parsed, so key
    /// redaction fell back to the text path and an object under a sensitive key leaked.
    #[test]
    fn a_sixteen_digit_integer_keeps_the_output_valid_and_key_redaction_on() {
        let r = Redactor::default();
        let out = r.redact(
            r#"{"created_us":1730000000000000,"credentials":{"user":"a","pw":"hunter2"},"n":1}"#,
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["credentials"], "[REDACTED]", "{out}");
        assert_eq!(v["created_us"], "[REDACTED]", "{out}");
        assert_eq!(v["n"], 1, "{out}");
        let out = r.redact(r#"{"created_us":1730000000000000,"secret":["hunter2"]}"#);
        assert!(!out.contains("hunter2"), "{out}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&out).is_ok(),
            "{out}"
        );
        // Nested in a string (an IPC body).
        let inner = r#"{"ts":1730000000000000,"password":{"v":"hunter2"}}"#;
        let out = r.redact(&serde_json::json!({ "response": inner }).to_string());
        assert!(!out.contains("hunter2"), "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(
            serde_json::from_str::<serde_json::Value>(v["response"].as_str().unwrap()).is_ok(),
            "{out}"
        );
        // A bare number result.
        assert_eq!(r.redact("1730000000000000"), r#""[REDACTED]""#);
        // Below the pattern, and clean JSON comes back byte for byte.
        let clean = r#"{"created_us": 173000000000000, "n": 1}"#;
        assert_eq!(r.redact(clean), clean);
    }

    /// Value patterns still reach everything the raw-text pass reached.
    #[test]
    fn value_patterns_cover_strings_keys_and_custom_patterns_across_structure() {
        let r = Redactor::default();
        let out = r.redact(r#"{"note":"mail ann@example.com","ann@example.com":1,"a":["sk-abc123def456ghi789jkl012mno"]}"#);
        assert!(!out.contains("ann@example.com"), "{out}");
        assert!(!out.contains("sk-abc123"), "{out}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&out).is_ok(),
            "{out}"
        );
        // A custom pattern written against the JSON text keeps working (fails closed).
        let r = Redactor::new(&[r#""internal":\s*"\w+""#.to_string()]);
        let out = r.redact(r#"{"internal":"abc","x":1}"#);
        assert!(!out.contains("abc"), "{out}");
        // A custom pattern matching a number leaf keeps the JSON valid.
        let r = Redactor::new(&[r"\b4242\b".to_string()]);
        let out = r.redact(r#"{"pin":4242}"#);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).expect("valid JSON")["pin"],
            "[REDACTED]"
        );
    }

    /// `[{name, value}]` (cookies) and `{key, value}` carry the sensitive word in a sibling field.
    #[test]
    fn name_value_pairs_are_redacted_by_their_name() {
        let r = Redactor::default();
        let out = r.redact(r#"[{"name":"session_token","value":"abc123opaque"},{"name":"theme","value":"dark"},{"key":"api_key","value":"k1"}]"#);
        assert!(
            !out.contains("abc123opaque") && !out.contains("k1"),
            "{out}"
        );
        assert!(
            out.contains("dark") && out.contains("session_token"),
            "{out}"
        );
    }

    #[test]
    fn short_credential_names_match_as_whole_words_only() {
        let r = Redactor::default();
        for (input, secret) in [
            (r#"{"pwd":"s3cr3t"}"#, "s3cr3t"),
            (r#"{"db_pwd":"s3cr3t"}"#, "s3cr3t"),
            (r#"{"passphrase":"s3cr3t"}"#, "s3cr3t"),
            (r#"{"jwt":"s3cr3t"}"#, "s3cr3t"),
            (r#"{"auth":"s3cr3t"}"#, "s3cr3t"),
            (r#"{"x-auth":"s3cr3t"}"#, "s3cr3t"),
            (r#"{"auth_header":"s3cr3t"}"#, "s3cr3t"),
            (r#"text {"pwd":"s3cr3t"} more"#, "s3cr3t"),
            (r#"text {"auth":"s3cr3t"} more"#, "s3cr3t"),
            ("connect pwd=s3cr3t now", "s3cr3t"),
            ("connect auth=s3cr3t now", "s3cr3t"),
        ] {
            let out = r.redact(input);
            assert!(!out.contains(secret), "{input} -> {out}");
        }
        for clean in [
            r#"{"author":"Ann","authority":"x","oauth_provider":"y"}"#,
            "author=Ann",
        ] {
            let out = r.redact(clean);
            assert!(out.contains("Ann"), "{clean} -> {out}");
        }
    }

    /// Two keys that redact to the same text used to collapse into one, losing a value.
    #[test]
    fn keys_redacted_to_the_same_text_keep_their_values() {
        let r = Redactor::default();
        let out = r.redact(r#"{"ann@example.com":1,"bob@example.com":2,"n":3}"#);
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(!out.contains("example.com"), "{out}");
        let map = v.as_object().unwrap();
        assert_eq!(map.len(), 3, "{out}");
        let mut values: Vec<i64> = map.values().map(|n| n.as_i64().unwrap()).collect();
        values.sort_unstable();
        assert_eq!(values, [1, 2, 3], "{out}");
    }

    /// `RegexSet::new` fails as a whole, so one bad pattern used to drop the good ones too.
    #[test]
    fn new_keeps_the_valid_custom_patterns_when_one_is_invalid() {
        let r = Redactor::new(&["ACME_\\w+".to_string(), "[invalid".to_string()]);
        assert_eq!(r.redact("found ACME_SECRET here"), "found [REDACTED] here");
    }

    #[test]
    fn try_new_valid_patterns() {
        let r = Redactor::try_new(&["secret_\\w+".to_string()]);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.redact("found secret_alpha here").contains("[REDACTED]"));
    }

    #[test]
    fn try_new_invalid_pattern_returns_error() {
        let r = Redactor::try_new(&["[invalid".to_string()]);
        assert!(r.is_err());
    }

    #[test]
    fn try_new_empty_patterns() {
        let r = Redactor::try_new(&[]);
        assert!(r.is_ok());
    }
}
