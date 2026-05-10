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
                let empty_set =
                    RegexSet::new(&empty).unwrap_or_else(|_| unreachable!());
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
    #[must_use]
    pub fn redact(&self, input: &str) -> String {
        let mut output = self.redact_regex(input);
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
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(input) {
            if !json_has_sensitive_keys(&value) {
                return input.to_string();
            }
            let redacted = redact_json_value(&value);
            serde_json::to_string(&redacted).unwrap_or_else(|_| input.to_string())
        } else {
            input.to_string()
        }
    }
}

fn json_has_sensitive_keys(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                let lower_key = key.to_lowercase();
                if SENSITIVE_JSON_KEYS
                    .iter()
                    .any(|k| lower_key.contains(&k.to_lowercase()))
                {
                    return true;
                }
                if json_has_sensitive_keys(val) {
                    return true;
                }
            }
            false
        }
        serde_json::Value::Array(arr) => arr.iter().any(json_has_sensitive_keys),
        _ => false,
    }
}

fn redact_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (key, val) in map {
                let lower_key = key.to_lowercase();
                if SENSITIVE_JSON_KEYS
                    .iter()
                    .any(|k| lower_key.contains(&k.to_lowercase()))
                {
                    if val.is_string() || val.is_number() {
                        new_map.insert(key.clone(), serde_json::Value::String("[REDACTED]".into()));
                    } else if val.is_boolean() {
                        // Booleans like has_api_key: true are safe — they indicate presence, not value
                        new_map.insert(key.clone(), val.clone());
                    } else {
                        new_map.insert(key.clone(), serde_json::Value::String("[REDACTED]".into()));
                    }
                } else {
                    new_map.insert(key.clone(), redact_json_value(val));
                }
            }
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(redact_json_value).collect())
        }
        other => other.clone(),
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
