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
];

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
            r#"(?i)("[^"\\\n]{{0,64}}?(?:{})[^"\\\n]{{0,64}}?"\s*:\s*)("(?:[^"\\]|\\.)*(?:"|\z)|-?\d[\d.eE+-]*)"#,
            words.join("|")
        ))
    },
);

/// Compile one of the built-in patterns; a failure is a bug, logged instead of panicking.
fn compile_builtin(pattern: &str) -> Option<regex::Regex> {
    regex::Regex::new(pattern)
        .inspect_err(|e| tracing::error!("BUG: built-in redaction pattern failed to compile: {e}"))
        .ok()
}

/// Replace the value of every sensitive `"key": value` pair found in free text.
fn redact_text_key_values(text: &str) -> std::borrow::Cow<'_, str> {
    match TEXT_KEY_VALUE_RE.as_ref() {
        Some(re) => re.replace_all(text, r#"${1}"[REDACTED]""#),
        None => std::borrow::Cow::Borrowed(text),
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
            match RegexSet::new(custom_patterns) {
                Ok(set) => {
                    let compiled: Vec<regex::Regex> = custom_patterns
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
    /// Passes, in order: the value patterns (API keys, bearer tokens, JWTs, emails, … plus any
    /// custom patterns); credential parameters in URLs (`?access_token=…`, `&sig=…`); then keys.
    /// Key-based redaction applies to the whole output when it is JSON — including JSON carried
    /// inside its string values (IPC/network bodies are JSON-encoded strings) — and otherwise to
    /// `"key": value` pairs found in the text. Best effort: see the limits in `docs/src/security.md`.
    #[must_use]
    pub fn redact(&self, input: &str) -> String {
        let mut output = self.redact_regex(input);
        if let Some(re) = URL_SECRET_PARAM_RE.as_ref()
            && re.is_match(&output)
        {
            output = re.replace_all(&output, "${1}[REDACTED]").into_owned();
        }
        output = self.redact_json_keys(&output);
        output
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

    fn redact_json_keys(&self, input: &str) -> String {
        match serde_json::from_str::<serde_json::Value>(input) {
            Ok(value) => match redact_json_value(&value, 0) {
                Some(redacted) => {
                    serde_json::to_string(&redacted).unwrap_or_else(|_| input.to_string())
                }
                None => input.to_string(),
            },
            // Not JSON as a whole: redact the `"key": value` pairs of any JSON inside the text.
            Err(_) => redact_text_key_values(input).into_owned(),
        }
    }
}

/// How deep JSON nested inside JSON strings is followed.
const MAX_NESTED_JSON_DEPTH: usize = 4;

fn is_sensitive_key(key: &str) -> bool {
    let lower_key = key.to_lowercase();
    SENSITIVE_JSON_KEYS
        .iter()
        .any(|k| lower_key.contains(&k.to_lowercase()))
}

/// `value` with every sensitive key's value redacted, or `None` if nothing needed redacting.
/// A string value that holds JSON is redacted inside (and re-encoded); any other string has the
/// `"key": value` pairs in its text redacted (a truncated JSON body, say).
fn redact_json_value(value: &serde_json::Value, depth: usize) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            let mut changed = false;
            let mut new_map = serde_json::Map::new();
            for (key, val) in map {
                let new_val = if is_sensitive_key(key) && !val.is_boolean() {
                    // Booleans like `has_api_key: true` only indicate presence — kept.
                    Some(serde_json::Value::String("[REDACTED]".into()))
                } else {
                    redact_json_value(val, depth)
                };
                changed |= new_val.is_some();
                new_map.insert(key.clone(), new_val.unwrap_or_else(|| val.clone()));
            }
            changed.then_some(serde_json::Value::Object(new_map))
        }
        serde_json::Value::Array(arr) => {
            let redacted: Vec<Option<serde_json::Value>> =
                arr.iter().map(|v| redact_json_value(v, depth)).collect();
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
        serde_json::Value::String(text) => redact_json_string(text, depth),
        _ => None,
    }
}

fn redact_json_string(text: &str, depth: usize) -> Option<serde_json::Value> {
    let trimmed = text.trim_start();
    if depth < MAX_NESTED_JSON_DEPTH
        && (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(inner) = serde_json::from_str::<serde_json::Value>(text)
    {
        let redacted = redact_json_value(&inner, depth + 1)?;
        return serde_json::to_string(&redacted)
            .ok()
            .map(serde_json::Value::String);
    }
    match redact_text_key_values(text) {
        std::borrow::Cow::Owned(redacted) => Some(serde_json::Value::String(redacted)),
        std::borrow::Cow::Borrowed(_) => None,
    }
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
