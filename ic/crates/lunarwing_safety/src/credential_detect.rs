//! Credential detection and display/storage redaction for HTTP parameters.
//!
//! Used by the built-in HTTP tool to decide whether approval is needed when
//! the LLM provides auth data directly in headers or URL query parameters.
//! The redaction helpers also cover credential-keyed JSON and form bodies.

/// Check whether HTTP request parameters contain manually-provided credentials.
///
/// Inspects headers (name/value), URL query parameters, and URL userinfo
/// for patterns that indicate authentication data.
pub fn params_contain_manual_credentials(params: &serde_json::Value) -> bool {
    headers_contain_credentials(params)
        || url_contains_credential_params(params)
        || url_contains_userinfo(params)
}

/// Header names that are exact matches for credential-carrying headers (case-insensitive).
const AUTH_HEADER_EXACT: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "api-key",
    "x-auth-token",
    "x-token",
    "x-access-token",
    "x-session-token",
    "x-csrf-token",
    "x-secret",
    "x-api-secret",
];

/// Substrings in header names that suggest credentials (case-insensitive).
/// Note: "key" is excluded to avoid false positives like "X-Idempotency-Key".
const AUTH_HEADER_SUBSTRINGS: &[&str] = &["auth", "token", "secret", "credential", "password"];

/// Value prefixes that indicate auth schemes (case-insensitive).
const AUTH_VALUE_PREFIXES: &[&str] = &[
    "bearer ",
    "basic ",
    "token ",
    "digest ",
    "hoba ",
    "mutual ",
    "aws4-hmac-sha256 ",
];

/// URL query parameter names that are exact matches for credentials (case-insensitive).
const AUTH_QUERY_EXACT: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "access_token",
    "token",
    "key",
    "secret",
    "password",
    "auth",
    "auth_token",
    "session_token",
    "client_secret",
    "client_id",
    "app_key",
    "app_secret",
    "sig",
    "signature",
];

/// Substrings in query parameter names that suggest credentials (case-insensitive).
const AUTH_QUERY_SUBSTRINGS: &[&str] = &["token", "secret", "auth", "password", "credential"];

fn header_name_is_credential(name: &str) -> bool {
    let lower = name.to_lowercase();

    if AUTH_HEADER_EXACT.contains(&lower.as_str()) {
        return true;
    }

    AUTH_HEADER_SUBSTRINGS.iter().any(|sub| lower.contains(sub))
}

/// WI#359 (CJ-11): public view of the header-name credential heuristic so
/// the engine's event redaction can blank credential-like header values.
pub fn header_name_carries_credentials(name: &str) -> bool {
    header_name_is_credential(name)
}

/// WI#359 (CJ-11): public view of the query-parameter credential heuristic
/// so URL sanitizers can redact credential-like query values.
pub fn query_param_carries_credentials(name: &str) -> bool {
    query_param_is_credential(name)
}

/// WI#359 (run-91 FI-2): public view of the header-value credential
/// heuristic (auth-scheme prefixes such as `Bearer ` or `Basic `) so the
/// engine's event redaction blanks a credential header even when its name
/// is innocuous — the approval gate already reacts to these values.
pub fn header_value_carries_credentials(value: &str) -> bool {
    header_value_is_credential(value)
}

/// Substrings matched against the normalized parameter key (lowercase, `_`
/// and `-` stripped, so `apiKey`/`api_key`/`api-key` all fold to `apikey`).
/// WI#359 (CJ-10): one shared credential-key list for the engine's event
/// redaction/summaries and the bridge's mission-hook redaction.
/// Deliberately broad — this is a redaction path, where over-redaction
/// costs display quality (e.g. an `author` key matches `auth`) and
/// under-redaction leaks credentials into persisted events.
const CREDENTIAL_KEY_SUBSTRINGS: &[&str] = &[
    "secret",
    "token",
    "passw",
    "apikey",
    "auth",
    "credential",
    "privatekey",
    "cookie",
    "bearer",
    "signature",
];

/// Whether a parameter key commonly carries credentials (WI#359, CJ-10).
///
/// Normalizes the key (lowercase, `_`/`-` stripped) and substring-matches
/// against [`CREDENTIAL_KEY_SUBSTRINGS`], so `hmac_secret`, `access_token`,
/// `client_secret`, `private_key`, `refresh_token`, and `apiKey` all match.
pub fn is_credential_param_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    CREDENTIAL_KEY_SUBSTRINGS
        .iter()
        .any(|sub| normalized.contains(sub))
}

/// Return a display/storage copy of `params` with every credential-keyed
/// value replaced by `"[REDACTED]"`, recursing into nested objects and
/// arrays (WI#359, CJ-10 — a positional `mission_update({…})` payload hides
/// keys under `_args[0]`, so a top-level-only pass misses them).
///
/// `null` and empty-string values are left untouched: redacting them would
/// claim a credential was present where there is none (WI#359 info item).
pub fn redact_credential_params(params: &serde_json::Value) -> serde_json::Value {
    match params {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let redacted = if is_credential_param_key(key)
                        && !value.is_null()
                        && !matches!(value, serde_json::Value::String(s) if s.is_empty())
                    {
                        serde_json::Value::String("[REDACTED]".into())
                    } else {
                        redact_credential_params(value)
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_credential_params).collect())
        }
        _ => params.clone(),
    }
}

/// Return a copy of `url_str` with credential material removed (WI#359,
/// CJ-11): userinfo stripped, credential-like query parameter values
/// replaced by `[REDACTED]`, and the fragment dropped (run-91 FI-4 — the
/// fragment never reaches the server but can carry credentials, e.g. the
/// OAuth implicit flow's `#access_token=…`). Unparseable URLs cannot be
/// analyzed, so only the portion before any `?`/`#` is kept.
///
/// The query is rewritten pair-by-pair from the raw string so untouched
/// pairs survive byte-for-byte (run-91 FI-7): rebuilding through the
/// `url` crate's serializer would render the marker as `%5BREDACTED%5D`,
/// turn `%20` into `+`, and grow `=` on flag params. A credential-named
/// pair with an empty value (`?api_key=`) is left alone — redacting it
/// would falsely claim a credential was present.
pub fn sanitize_url_credentials(url_str: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url_str) else {
        return url_str
            .split(['?', '#'])
            .next()
            .unwrap_or(url_str)
            .to_string();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_fragment(None);
    match parsed.query() {
        None | Some("") => parsed.set_query(None),
        Some(raw) => {
            let redacted = redact_query_credentials(raw);
            parsed.set_query(Some(&redacted));
        }
    }
    parsed.to_string()
}

/// Redact credential values in a form-encoded HTTP body (run-97 FS-1).
///
/// Uses the same decoded-key heuristics as URL queries, preserving untouched
/// pairs, empty values and free text byte-for-byte. This is a display/storage
/// copy; the original body remains available for execution.
pub fn redact_form_credentials(body: &str) -> String {
    redact_query_credentials(body)
}

/// Redact credential values in a raw query string, preserving every
/// untouched pair byte-for-byte. The key check unions the query heuristic
/// with the shared credential-key list (run-91 FI-3), so names like
/// `x-api-key`, `private_key`, or `X-Amz-Signature` are caught too.
fn redact_query_credentials(raw_query: &str) -> String {
    raw_query
        .split('&')
        .map(|pair| {
            let (raw_name, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
            let name = decode_query_component(raw_name);
            let credential =
                query_param_carries_credentials(&name) || is_credential_param_key(&name);
            if credential && !raw_value.is_empty() {
                format!("{raw_name}=[REDACTED]")
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Decode one raw query component (percent-escapes and `+`) so the key
/// check sees the same name the server would; the raw form is what gets
/// re-emitted.
fn decode_query_component(raw: &str) -> String {
    url::form_urlencoded::parse(format!("{raw}=").as_bytes())
        .next()
        .map(|(name, _)| name.into_owned())
        .unwrap_or_else(|| raw.to_string())
}

fn header_value_is_credential(value: &str) -> bool {
    let lower = value.to_lowercase();
    AUTH_VALUE_PREFIXES.iter().any(|pfx| lower.starts_with(pfx))
}

fn headers_contain_credentials(params: &serde_json::Value) -> bool {
    match params.get("headers") {
        Some(serde_json::Value::Object(map)) => map.iter().any(|(k, v)| {
            header_name_is_credential(k) || v.as_str().is_some_and(header_value_is_credential)
        }),
        Some(serde_json::Value::Array(items)) => items.iter().any(|item| {
            let name_match = item
                .get("name")
                .and_then(|n| n.as_str())
                .is_some_and(header_name_is_credential);
            let value_match = item
                .get("value")
                .and_then(|v| v.as_str())
                .is_some_and(header_value_is_credential);
            name_match || value_match
        }),
        _ => false,
    }
}

fn query_param_is_credential(name: &str) -> bool {
    let lower = name.to_lowercase();

    if AUTH_QUERY_EXACT.contains(&lower.as_str()) {
        return true;
    }

    AUTH_QUERY_SUBSTRINGS.iter().any(|sub| lower.contains(sub))
}

fn url_contains_credential_params(params: &serde_json::Value) -> bool {
    let url_str = match params.get("url").and_then(|u| u.as_str()) {
        Some(u) => u,
        None => return false,
    };

    let parsed = match url::Url::parse(url_str) {
        Ok(u) => u,
        Err(_) => return false,
    };

    parsed
        .query_pairs()
        .any(|(name, _)| query_param_is_credential(&name))
}

/// Detect credentials embedded in URL userinfo (e.g., `https://user:pass@host/`).
fn url_contains_userinfo(params: &serde_json::Value) -> bool {
    let url_str = match params.get("url").and_then(|u| u.as_str()) {
        Some(u) => u,
        None => return false,
    };

    let parsed = match url::Url::parse(url_str) {
        Ok(u) => u,
        Err(_) => return false,
    };

    // Non-empty username or password in the URL indicates embedded credentials
    !parsed.username().is_empty() || parsed.password().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Header name exact match ────────────────────────────────────────

    #[test]
    fn test_authorization_header_detected() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://api.example.com",
            "headers": {"Authorization": "Bearer token123"}
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_all_exact_header_names() {
        for name in AUTH_HEADER_EXACT {
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://example.com",
                "headers": {name.to_string(): "some_value"}
            });
            assert!(
                params_contain_manual_credentials(&params),
                "Header '{}' should be detected",
                name
            );
        }
    }

    #[test]
    fn test_header_name_case_insensitive() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": {"AUTHORIZATION": "value"}
        });
        assert!(params_contain_manual_credentials(&params));
    }

    // ── Header name substring match ────────────────────────────────────

    #[test]
    fn test_header_substring_auth() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": {"X-Custom-Auth-Header": "value"}
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_header_substring_token() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": {"X-My-Token": "value"}
        });
        assert!(params_contain_manual_credentials(&params));
    }

    // ── Header value prefix match ──────────────────────────────────────

    #[test]
    fn test_bearer_value_detected() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": {"X-Custom": "Bearer sk-abc123"}
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_basic_value_detected() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": {"X-Custom": "Basic dXNlcjpwYXNz"}
        });
        assert!(params_contain_manual_credentials(&params));
    }

    // ── Array-format headers ───────────────────────────────────────────

    #[test]
    fn test_array_format_header_name() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": [{"name": "Authorization", "value": "Bearer token"}]
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_array_format_header_value_prefix() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": [{"name": "X-Custom", "value": "Token abc123"}]
        });
        assert!(params_contain_manual_credentials(&params));
    }

    // ── URL query parameter detection ──────────────────────────────────

    #[test]
    fn test_url_api_key_param() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://api.example.com/data?api_key=abc123"
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_url_access_token_param() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://api.example.com/data?access_token=xyz"
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_url_query_substring_match() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://api.example.com/data?my_auth_code=xyz"
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_url_query_case_insensitive() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://api.example.com/data?API_KEY=abc"
        });
        assert!(params_contain_manual_credentials(&params));
    }

    // ── False positive checks ──────────────────────────────────────────

    #[test]
    fn test_idempotency_key_not_detected() {
        let params = serde_json::json!({
            "method": "POST",
            "url": "https://api.example.com",
            "headers": {"X-Idempotency-Key": "uuid-1234"}
        });
        assert!(!params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_content_type_not_detected() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": {"Content-Type": "application/json", "Accept": "text/html"}
        });
        assert!(!params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_no_headers_no_query() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com/path"
        });
        assert!(!params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_safe_query_params() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://api.example.com/search?q=hello&page=1&limit=10"
        });
        assert!(!params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_empty_headers() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "headers": {}
        });
        assert!(!params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_invalid_url_returns_false() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "not a url"
        });
        assert!(!params_contain_manual_credentials(&params));
    }

    // ── URL userinfo detection ─────────────────────────────────────────

    #[test]
    fn test_url_userinfo_with_password_detected() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://user:pass@api.example.com/data"
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_url_userinfo_username_only_detected() {
        let params = serde_json::json!({
            "method": "GET",
            "url": "https://apikey@api.example.com/data"
        });
        assert!(params_contain_manual_credentials(&params));
    }

    #[test]
    fn test_url_without_userinfo_not_detected_by_userinfo_check() {
        // This specifically tests that url_contains_userinfo returns false
        // for a normal URL (the broader function may still detect query params).
        assert!(!url_contains_userinfo(&serde_json::json!({
            "url": "https://api.example.com/data"
        })));
    }

    // ── WI#359 (CJ-10): shared credential-key predicate ────────────────

    #[test]
    fn credential_param_key_normalizes_and_substring_matches() {
        for key in [
            "api_key",
            "apiKey",
            "api-key",
            "webhook_secret",
            "hmac_secret",
            "access_token",
            "client_secret",
            "private_key",
            "refresh_token",
            "Authorization",
            "x_auth",
            "session_cookie",
            "user_password",
            "bearer",
            "request_signature",
            "aws_credentials",
        ] {
            assert!(is_credential_param_key(key), "'{key}' must match");
        }
        for key in ["url", "name", "id", "goal", "query", "content", "path"] {
            assert!(!is_credential_param_key(key), "'{key}' must not match");
        }
    }

    #[test]
    fn redact_credential_params_recurses_and_preserves_non_credentials() {
        let params = serde_json::json!({
            "id": "mission-1",
            "hmac_secret": "s3cr3t",
            "_args": [{"webhook_secret": "nested-secret", "name": "ok"}],
            "headers": [{"name": "Authorization", "value": "Bearer raw"}],
        });
        let redacted = redact_credential_params(&params);
        assert_eq!(redacted["hmac_secret"], "[REDACTED]");
        assert_eq!(redacted["_args"][0]["webhook_secret"], "[REDACTED]");
        assert_eq!(redacted["_args"][0]["name"], "ok");
        assert_eq!(redacted["id"], "mission-1");
        // Array-item "name" keys are not credential keys; the http-specific
        // pass handles header values (redact_event_params).
        assert_eq!(redacted["headers"][0]["value"], "Bearer raw");
    }

    #[test]
    fn redact_credential_params_leaves_null_and_empty_values_alone() {
        // WI#359 info item: redacting null/"" would falsely claim a
        // credential was present.
        let params = serde_json::json!({"webhook_secret": null, "api_key": ""});
        let redacted = redact_credential_params(&params);
        assert_eq!(redacted["webhook_secret"], serde_json::Value::Null);
        assert_eq!(redacted["api_key"], "");
    }

    // ── WI#359 (CJ-11): URL credential sanitization ────────────────────

    #[test]
    fn sanitize_url_redacts_credential_query_values_and_userinfo() {
        let sanitized = sanitize_url_credentials(
            "https://user:pass@api.example.com/data?api_key=abc123&page=2&sig=deadbeef",
        );
        assert!(
            !sanitized.contains("abc123"),
            "api_key value leaked: {sanitized}"
        );
        assert!(
            !sanitized.contains("deadbeef"),
            "sig value leaked: {sanitized}"
        );
        assert!(
            !sanitized.contains("user:pass"),
            "userinfo leaked: {sanitized}"
        );
        assert!(
            sanitized.contains("page=2"),
            "safe param dropped: {sanitized}"
        );
        assert!(sanitized.starts_with("https://api.example.com/data"));
    }

    #[test]
    fn sanitize_url_unparseable_drops_query_and_fragment() {
        assert_eq!(sanitize_url_credentials("not a url?api_key=x"), "not a url");
        assert_eq!(sanitize_url_credentials("plain"), "plain");
    }

    // ── WI#359 run-91 (FI-2/FI-3/FI-4/FI-7) ────────────────────────────

    #[test]
    fn header_value_carries_credentials_matches_auth_scheme_prefixes() {
        // FI-2: the approval gate reacts to these values, so redaction must
        // recognize them even under an innocuous header name.
        assert!(header_value_carries_credentials("Bearer abc123"));
        assert!(header_value_carries_credentials("Basic dXNlcjpwYXNz"));
        assert!(header_value_carries_credentials(
            "AWS4-HMAC-SHA256 Credential=x"
        ));
        assert!(!header_value_carries_credentials("application/json"));
        assert!(!header_value_carries_credentials(""));
    }

    #[test]
    fn sanitize_url_drops_the_fragment() {
        // FI-4: the fragment can carry credentials (OAuth implicit flow)
        // and never reaches the server, so it is dropped, matching the
        // unparseable-URL branch.
        let sanitized = sanitize_url_credentials("https://h.example/cb#access_token=X");
        assert_eq!(sanitized, "https://h.example/cb");
        let sanitized = sanitize_url_credentials("https://h.example/cb?token=X#frag?api_key=Y");
        assert!(
            !sanitized.contains('Y'),
            "fragment query leaked: {sanitized}"
        );
        assert!(!sanitized.contains('X'), "token value leaked: {sanitized}");
        assert!(!sanitized.contains('#'), "fragment kept: {sanitized}");
    }

    #[test]
    fn sanitize_url_query_uses_the_shared_credential_key_list() {
        // FI-3: the query check unions the query heuristic with the shared
        // credential-key list.
        let sanitized = sanitize_url_credentials(
            "https://api.example.com/d?x-api-key=V&private_key=W&X-Amz-Signature=S&q=hello",
        );
        for leaked in ["V", "W", "S"] {
            assert!(
                !sanitized.contains(&format!("={leaked}")),
                "value {leaked} leaked: {sanitized}"
            );
        }
        assert!(
            sanitized.contains("q=hello"),
            "safe param dropped: {sanitized}"
        );
    }

    #[test]
    fn sanitize_url_preserves_untouched_pairs_byte_for_byte() {
        // FI-7: no re-encoding drift, the marker renders literally, an empty
        // credential value is left alone (redacting it would falsely claim
        // a credential was present), and flag params keep their bare form.
        let sanitized =
            sanitize_url_credentials("https://api.example.com/d?q=a%20b&flag&api_key=&token=abc");
        assert!(sanitized.contains("q=a%20b"), "encoding drift: {sanitized}");
        assert!(
            sanitized.contains("&flag&"),
            "flag param grew '=': {sanitized}"
        );
        assert!(
            sanitized.contains("api_key=&"),
            "empty value redacted: {sanitized}"
        );
        assert!(
            sanitized.contains("token=[REDACTED]"),
            "marker percent-encoded or missing: {sanitized}"
        );
        assert!(
            !sanitized.contains("abc"),
            "token value leaked: {sanitized}"
        );
    }

    #[test]
    fn redact_form_credentials_preserves_safe_pairs_and_empty_values() {
        let body = "grant_type=refresh_token&client_secret=form-secret&refresh_token=form-token\
                    &api%5Fkey=encoded-key&scope=read%20write&flag&password=&client_secret=another";
        assert_eq!(
            redact_form_credentials(body),
            "grant_type=refresh_token&client_secret=[REDACTED]&refresh_token=[REDACTED]\
             &api%5Fkey=[REDACTED]&scope=read%20write&flag&password=&client_secret=[REDACTED]"
        );
    }

    #[test]
    fn redact_form_credentials_preserves_plain_text_and_empty_bodies() {
        for body in ["", "plain text body", "a=1&b=two+words", "flag&empty="] {
            assert_eq!(redact_form_credentials(body), body);
        }
    }

    /// Adversarial tests for credential detection with Unicode, control chars,
    /// and case folding edge cases.
    mod adversarial {
        use super::*;

        // ── B. Unicode edge cases ────────────────────────────────────

        #[test]
        fn header_name_with_zwsp_not_detected() {
            // ZWSP in header name: "Author\u{200B}ization" is NOT "Authorization"
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://example.com",
                "headers": {"Author\u{200B}ization": "Bearer token123"}
            });
            // The header NAME won't match exact "authorization" due to ZWSP.
            // But the VALUE still starts with "Bearer " — so value check catches it.
            assert!(
                params_contain_manual_credentials(&params),
                "Bearer prefix in value should still be detected even with ZWSP in header name"
            );
        }

        #[test]
        fn bearer_prefix_with_zwsp_bypass() {
            // ZWSP inside "Bearer": "Bear\u{200B}er token123"
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://example.com",
                "headers": {"X-Custom": "Bear\u{200B}er token123"}
            });
            // ZWSP breaks the "bearer " prefix match. Header name "X-Custom"
            // doesn't match exact/substring either. Documents bypass vector.
            let result = params_contain_manual_credentials(&params);
            // This should NOT be detected — documenting the limitation
            assert!(
                !result,
                "ZWSP in 'Bearer' prefix breaks detection — known limitation"
            );
        }

        #[test]
        fn rtl_override_in_url_query_param() {
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://api.example.com/data?\u{202E}api_key=secret"
            });
            // RTL override before "api_key" in query. url::Url::parse
            // percent-encodes the RTL char, making the query pair name
            // "%E2%80%AEapi_key" which does NOT match "api_key" exactly.
            // The substring check for "auth"/"token" also misses.
            // Document: RTL override can bypass query param detection.
            let result = params_contain_manual_credentials(&params);
            assert!(
                !result,
                "RTL override before query param name breaks detection — known limitation"
            );
        }

        #[test]
        fn zwnj_in_header_name() {
            // ZWNJ (\u{200C}) inserted into "Authorization"
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://example.com",
                "headers": {"Author\u{200C}ization": "some_value"}
            });
            // ZWNJ breaks the exact match for "authorization".
            // Substring check for "auth" still matches "author\u{200C}ization"
            // because to_lowercase preserves ZWNJ and "auth" appears before it.
            assert!(
                params_contain_manual_credentials(&params),
                "ZWNJ in header name — substring 'auth' check should still catch it"
            );
        }

        #[test]
        fn emoji_in_url_path_does_not_panic() {
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://api.example.com/🔑?api_key=secret"
            });
            // url::Url::parse handles emoji in paths. Credential param should still detect.
            assert!(params_contain_manual_credentials(&params));
        }

        #[test]
        fn unicode_case_folding_turkish_i() {
            // Turkish İ (U+0130) lowercases to "i̇" (i + combining dot above)
            // in Unicode, but to_lowercase() in Rust follows Unicode rules.
            // "Authorization" with Turkish İ: "Authorİzation"
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://example.com",
                "headers": {"Author\u{0130}zation": "value"}
            });
            // to_lowercase() of İ is "i̇" (2 chars), so "authorİzation" becomes
            // "authori̇zation" — does NOT match "authorization".
            // The substring check for "auth" WILL match though.
            assert!(
                params_contain_manual_credentials(&params),
                "Turkish İ — substring 'auth' check should still catch it"
            );
        }

        #[test]
        fn multibyte_userinfo_in_url() {
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://用户:密码@api.example.com/data"
            });
            // Non-ASCII username/password in URL userinfo
            assert!(
                params_contain_manual_credentials(&params),
                "multibyte userinfo should be detected"
            );
        }

        // ── C. Control character variants ────────────────────────────

        #[test]
        fn control_chars_in_header_name_still_detects() {
            for byte in [0x01u8, 0x02, 0x0B, 0x1F] {
                let name = format!("Authorization{}", char::from(byte));
                let params = serde_json::json!({
                    "method": "GET",
                    "url": "https://example.com",
                    "headers": {name: "Bearer token"}
                });
                // Header name contains "auth" substring, and value starts with
                // "Bearer " — both checks should still work with trailing control char.
                assert!(
                    params_contain_manual_credentials(&params),
                    "control char 0x{:02X} appended to header name should not prevent detection",
                    byte
                );
            }
        }

        #[test]
        fn control_chars_in_header_value_breaks_prefix() {
            for byte in [0x01u8, 0x02, 0x0B, 0x1F] {
                let value = format!("Bearer{}token123456789012345", char::from(byte));
                let params = serde_json::json!({
                    "method": "GET",
                    "url": "https://example.com",
                    "headers": {"Authorization": value}
                });
                // Header name "Authorization" is an exact match — always detected
                // regardless of value content. No panic is secondary assertion.
                assert!(
                    params_contain_manual_credentials(&params),
                    "Authorization header name should be detected regardless of value content"
                );
            }
        }

        #[test]
        fn bom_prefix_in_url() {
            let params = serde_json::json!({
                "method": "GET",
                "url": "\u{FEFF}https://api.example.com/data?api_key=secret"
            });
            // BOM before "https://" makes url::Url::parse fail, so
            // query param detection returns false. Document this.
            let result = params_contain_manual_credentials(&params);
            assert!(
                !result,
                "BOM prefix makes URL unparseable — query param detection fails (known limitation)"
            );
        }

        #[test]
        fn null_byte_in_query_value() {
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://api.example.com/data?api_key=sec\x00ret"
            });
            // The param NAME "api_key" still matches regardless of value content.
            assert!(
                params_contain_manual_credentials(&params),
                "null byte in query value should not prevent param name detection"
            );
        }

        #[test]
        fn idn_unicode_hostname_with_credential_params() {
            // Internationalized domain name (IDN) with credential query param
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://例え.jp/api?api_key=secret123"
            });
            // url::Url::parse handles IDN. Credential param should still detect.
            assert!(
                params_contain_manual_credentials(&params),
                "IDN hostname should not prevent credential param detection"
            );
        }

        #[test]
        fn non_ascii_header_names_substring_detection() {
            // Header names with various non-ASCII characters — test both
            // detection behavior AND no-panic guarantee.
            let detected_cases = [
                ("🔑Auth", true),       // contains "auth" substring
                ("Autorización", true), // contains "auth" via to_lowercase
                ("Héader-Tökën", true), // contains "token" via "tökën"? No — "ö" ≠ "o"
            ];

            // These should NOT be detected — no auth substring
            let not_detected_cases = [
                "认证",        // Chinese — no ASCII substring match
                "Авторизация", // Russian — no ASCII substring match
            ];

            for name in not_detected_cases {
                let params = serde_json::json!({
                    "method": "GET",
                    "url": "https://example.com",
                    "headers": {name: "some_value"}
                });
                assert!(
                    !params_contain_manual_credentials(&params),
                    "non-ASCII header '{}' should not be detected (no ASCII auth substring)",
                    name
                );
            }

            // "🔑Auth" contains "auth" substring
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://example.com",
                "headers": {"🔑Auth": "some_value"}
            });
            assert!(
                params_contain_manual_credentials(&params),
                "emoji+Auth header should be detected via 'auth' substring"
            );

            // "Autorización" lowercases to "autorización" — does NOT contain
            // "auth" (it has "aut" + "o", not "auth"). Document this.
            let params = serde_json::json!({
                "method": "GET",
                "url": "https://example.com",
                "headers": {"Autorización": "some_value"}
            });
            assert!(
                !params_contain_manual_credentials(&params),
                "Spanish 'Autorización' does not contain 'auth' substring — not detected"
            );

            let _ = detected_cases; // suppress unused warning
        }
    }
}
