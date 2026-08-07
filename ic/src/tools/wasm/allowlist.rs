//! HTTP endpoint allowlist validation.
//!
//! Validates that HTTP requests from WASM tools only go to allowed endpoints.
//! This is the first line of defense against unauthorized API access.
//!
//! # Validation Flow
//!
//! ```text
//! WASM HTTP request ──► Parse URL ──► Check allowlist ──► Allow/Deny
//!                          │               │
//!                          │               ├─► Host match?
//!                          │               ├─► Path prefix match?
//!                          │               └─► Method allowed?
//!                          │
//!                          └─► Validate URL format
//! ```

use std::fmt;

use crate::tools::wasm::capabilities::EndpointPattern;

/// Result of allowlist validation.
#[derive(Debug, Clone)]
pub enum AllowlistResult {
    /// Request is allowed.
    Allowed,
    /// Request is denied with reason.
    Denied(DenyReason),
}

impl AllowlistResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, AllowlistResult::Allowed)
    }
}

/// Reason why a request was denied.
#[derive(Debug, Clone)]
pub enum DenyReason {
    /// URL could not be parsed.
    InvalidUrl(String),
    /// Host is not in the allowlist.
    HostNotAllowed(String),
    /// Path does not match any allowed prefix.
    PathNotAllowed { host: String, path: String },
    /// HTTP method is not allowed for this endpoint.
    MethodNotAllowed { method: String, host: String },
    /// Allowlist is empty (no endpoints configured).
    EmptyAllowlist,
    /// URL scheme is not HTTPS.
    InsecureScheme(String),
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenyReason::InvalidUrl(url) => write!(f, "Invalid URL: {}", url),
            DenyReason::HostNotAllowed(host) => write!(f, "Host not in allowlist: {}", host),
            DenyReason::PathNotAllowed { host, path } => {
                write!(f, "Path not allowed for host {}: {}", host, path)
            }
            DenyReason::MethodNotAllowed { method, host } => {
                write!(f, "Method {} not allowed for host {}", method, host)
            }
            DenyReason::EmptyAllowlist => write!(f, "No endpoints in allowlist"),
            DenyReason::InsecureScheme(scheme) => {
                write!(f, "Insecure scheme: {} (only HTTPS allowed)", scheme)
            }
        }
    }
}

/// Validates HTTP requests against an allowlist.
pub struct AllowlistValidator {
    patterns: Vec<EndpointPattern>,
    /// Whether to require HTTPS (default: true).
    require_https: bool,
}

impl AllowlistValidator {
    /// Create a new validator with the given patterns.
    pub fn new(patterns: Vec<EndpointPattern>) -> Self {
        Self {
            patterns,
            require_https: true,
        }
    }

    /// Allow HTTP (insecure) requests. Use with caution.
    pub fn allow_http(mut self) -> Self {
        self.require_https = false;
        self
    }

    /// Check if a request is allowed.
    pub fn validate(&self, url: &str, method: &str) -> AllowlistResult {
        // Check for empty allowlist
        if self.patterns.is_empty() {
            return AllowlistResult::Denied(DenyReason::EmptyAllowlist);
        }

        // Parse the URL
        let parsed = match parse_url(url) {
            Ok(p) => p,
            Err(e) => return AllowlistResult::Denied(DenyReason::InvalidUrl(e)),
        };

        // Check HTTPS requirement
        if self.require_https && parsed.scheme != "https" {
            return AllowlistResult::Denied(DenyReason::InsecureScheme(parsed.scheme.clone()));
        }

        // Find a matching pattern
        for pattern in &self.patterns {
            if pattern.matches(&parsed.host, &parsed.path, method) {
                return AllowlistResult::Allowed;
            }
        }

        // No pattern matched, figure out why for better error messages
        let host_matches: Vec<_> = self
            .patterns
            .iter()
            .filter(|p| p.host_matches(&parsed.host))
            .collect();

        if host_matches.is_empty() {
            AllowlistResult::Denied(DenyReason::HostNotAllowed(parsed.host))
        } else {
            // Host matches but path/method doesn't
            let path_matches: Vec<_> = host_matches
                .iter()
                .filter(|p| p.path_matches(&parsed.path))
                .collect();

            if path_matches.is_empty() {
                AllowlistResult::Denied(DenyReason::PathNotAllowed {
                    host: parsed.host,
                    path: parsed.path,
                })
            } else {
                AllowlistResult::Denied(DenyReason::MethodNotAllowed {
                    method: method.to_string(),
                    host: parsed.host,
                })
            }
        }
    }

    /// Check if any pattern would allow this host.
    pub fn host_allowed(&self, host: &str) -> bool {
        self.patterns.iter().any(|p| p.host_matches(host))
    }

    /// Get all allowed hosts (for debugging/logging).
    pub fn allowed_hosts(&self) -> Vec<&str> {
        self.patterns.iter().map(|p| p.host.as_str()).collect()
    }
}

/// Parsed URL components.
struct ParsedUrl {
    scheme: String,
    host: String,
    path: String,
}

/// Parse and normalize URL components for allowlist matching.
fn parse_url(url: &str) -> Result<ParsedUrl, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("URL parse failed: {e}"))?;
    let scheme = parsed.scheme().to_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!("Unsupported scheme: {}", scheme));
    }

    // Reject URLs with userinfo (user:pass@host) to prevent host-confusion bypasses.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URL contains userinfo (@) which is not allowed".to_string());
    }

    let host = parsed.host_str().ok_or_else(|| "Empty host".to_string())?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_lowercase();
    let normalized_path = normalize_path(parsed.path())?;

    Ok(ParsedUrl {
        scheme,
        host,
        path: normalized_path,
    })
}

fn normalize_path(path: &str) -> Result<String, String> {
    let mut segments: Vec<String> = Vec::new();
    for raw_segment in path.split('/') {
        if !has_valid_percent_encoding(raw_segment) {
            return Err(format!(
                "Invalid percent-encoding in path segment: {raw_segment}"
            ));
        }

        let segment = urlencoding::decode(raw_segment)
            .map_err(|_| format!("Invalid percent-encoding in path segment: {raw_segment}"))?;
        let segment = segment.as_ref();

        // Backslash stays rejected: it is an ambiguous separator on some
        // downstream stacks and has no legitimate use in allowlisted HTTPS
        // API paths.
        if segment.contains('\\') {
            return Err("Path segment contains encoded backslash separator".to_string());
        }

        // An encoded `/` (e.g. GitLab `group%2Fproject` or `feature%2Fbranch`
        // path parameters) is treated as a real separator so the allowlist
        // matches against the fully decoded, canonical path hierarchy.
        // Encoded dot segments collapse exactly like literal ones, so
        // traversal smuggling (`..%2F..`) stays blocked.
        for part in segment.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                _ => segments.push(part.to_string()),
            }
        }
    }

    let mut result = String::with_capacity(path.len().max(1));
    result.push('/');
    result.push_str(&segments.join("/"));
    if path.len() > 1 && path.ends_with('/') && !result.ends_with('/') {
        result.push('/');
    }
    Ok(result)
}

fn has_valid_percent_encoding(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return false;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::tools::wasm::allowlist::{AllowlistValidator, DenyReason};
    use crate::tools::wasm::capabilities::EndpointPattern;

    fn validator_with_patterns() -> AllowlistValidator {
        AllowlistValidator::new(vec![
            EndpointPattern::host("api.openai.com").with_path_prefix("/v1/"),
            EndpointPattern::host("api.anthropic.com")
                .with_path_prefix("/v1/messages")
                .with_methods(vec!["POST".to_string()]),
            EndpointPattern::host("*.example.com"),
        ])
    }

    #[test]
    fn test_allowed_request() {
        let validator = validator_with_patterns();

        let result = validator.validate("https://api.openai.com/v1/chat/completions", "POST");
        assert!(result.is_allowed());
    }

    #[test]
    fn test_denied_wrong_host() {
        let validator = validator_with_patterns();

        let result = validator.validate("https://evil.com/steal/data", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::HostNotAllowed(_)));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_denied_wrong_path() {
        let validator = validator_with_patterns();

        let result = validator.validate("https://api.openai.com/v2/different", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::PathNotAllowed { .. }));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_denied_wrong_method() {
        let validator = validator_with_patterns();

        // Anthropic endpoint only allows POST
        let result = validator.validate("https://api.anthropic.com/v1/messages", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::MethodNotAllowed { .. }));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_wildcard_host() {
        let validator = validator_with_patterns();

        let result = validator.validate("https://api.example.com/anything", "GET");
        assert!(result.is_allowed());

        let result = validator.validate("https://sub.api.example.com/anything", "GET");
        assert!(result.is_allowed());
    }

    #[test]
    fn test_require_https() {
        let validator = validator_with_patterns();

        let result = validator.validate("http://api.openai.com/v1/chat", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::InsecureScheme(_)));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_allow_http() {
        let validator = validator_with_patterns().allow_http();

        let result = validator.validate("http://api.example.com/test", "GET");
        assert!(result.is_allowed());
    }

    #[test]
    fn test_empty_allowlist() {
        let validator = AllowlistValidator::new(vec![]);

        let result = validator.validate("https://anything.com/", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::EmptyAllowlist));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_userinfo_rejected() {
        let validator = validator_with_patterns();

        // Userinfo in URL should be rejected to prevent allowlist bypass
        let result = validator.validate("https://api.openai.com@evil.com/v1/chat", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::InvalidUrl(_)));
        } else {
            panic!("Expected denied for userinfo URL");
        }
    }

    #[test]
    fn test_invalid_url() {
        let validator = validator_with_patterns();

        let result = validator.validate("not-a-url", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::InvalidUrl(_)));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_path_traversal_blocked() {
        let validator = validator_with_patterns();
        assert!(
            !validator
                .validate("https://api.openai.com/v1/../admin", "GET")
                .is_allowed()
        );
        assert!(
            !validator
                .validate("https://api.openai.com/v1/../../etc/passwd", "GET")
                .is_allowed()
        );
        assert!(
            !validator
                .validate("https://api.openai.com/v1/%2E%2E/admin", "GET")
                .is_allowed()
        );
        assert!(
            !validator
                .validate("https://api.openai.com/v1/%2e%2e/%2e%2e/root", "GET")
                .is_allowed()
        );
        assert!(
            validator
                .validate("https://api.openai.com/v1/chat/completions", "POST")
                .is_allowed()
        );
    }

    #[test]
    fn test_normalize_path() {
        use super::normalize_path;
        assert_eq!(normalize_path("/v1/../admin").unwrap(), "/admin");
        assert_eq!(
            normalize_path("/v1/chat/completions").unwrap(),
            "/v1/chat/completions"
        );
        assert_eq!(normalize_path("/v1/./chat").unwrap(), "/v1/chat");
        assert_eq!(
            normalize_path("/v1/../../../etc/passwd").unwrap(),
            "/etc/passwd"
        );
        assert_eq!(normalize_path("/v1/%2e%2e/admin").unwrap(), "/admin");
        assert_eq!(normalize_path("/").unwrap(), "/");
        assert_eq!(normalize_path("/v1/").unwrap(), "/v1/");
        // Encoded `/` decodes to a real separator before normalization.
        assert_eq!(
            normalize_path("/api/v4/projects/lunarwing%2Flunarwing2").unwrap(),
            "/api/v4/projects/lunarwing/lunarwing2"
        );
        assert_eq!(normalize_path("/v1/..%2F../root").unwrap(), "/root");
        assert!(normalize_path("/v1/%5Cadmin").is_err());
    }

    #[test]
    fn test_invalid_encoded_path_rejected() {
        let validator = validator_with_patterns();
        let result = validator.validate("https://api.openai.com/v1/%ZZ/chat", "GET");
        assert!(!result.is_allowed());
        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::InvalidUrl(_)));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_encoded_separator_normalized() {
        let validator = validator_with_patterns();
        // An encoded `/` inside a segment decodes to a real separator and the
        // allowlist matches the canonical decoded hierarchy.
        let result = validator.validate("https://api.openai.com/v1/%2Fadmin", "GET");
        assert!(result.is_allowed());

        // GitLab-style namespaced path parameters must work: branch names and
        // group/project paths are percent-encoded into a single segment.
        let gitlab = AllowlistValidator::new(vec![
            EndpointPattern::host("git.lunarwing.org").with_path_prefix("/api/v4/"),
        ]);
        let result = gitlab.validate(
            "https://git.lunarwing.org/api/v4/projects/35/repository/branches/dwarfstar%2Ftest-write-ops",
            "DELETE",
        );
        assert!(result.is_allowed());
        let result = gitlab.validate(
            "https://git.lunarwing.org/api/v4/projects/lunarwing%2Flunarwing2",
            "GET",
        );
        assert!(result.is_allowed());

        // Encoded separators cannot smuggle traversal past the prefix check:
        // `..%2F..` collapses two levels, leaving the prefix unmatched.
        let result = validator.validate("https://api.openai.com/v1/..%2F../root", "GET");
        assert!(!result.is_allowed());

        // Encoded backslash stays rejected as an ambiguous separator.
        let result = validator.validate("https://api.openai.com/v1/%5Cadmin", "GET");
        assert!(!result.is_allowed());
        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::InvalidUrl(_)));
        } else {
            panic!("Expected denied");
        }
    }

    #[test]
    fn test_percent_encoding_validator() {
        use super::has_valid_percent_encoding;
        assert!(has_valid_percent_encoding("%2F"));
        assert!(has_valid_percent_encoding("hello%20world"));
        assert!(!has_valid_percent_encoding("%"));
        assert!(!has_valid_percent_encoding("%2"));
        assert!(!has_valid_percent_encoding("%ZZ"));
    }

    #[test]
    fn test_url_with_port() {
        let validator =
            AllowlistValidator::new(vec![EndpointPattern::host("localhost")]).allow_http();

        let result = validator.validate("http://localhost:8080/api", "GET");
        assert!(result.is_allowed());
    }

    #[test]
    fn test_wildcard_path_prefix_base_path_deployment() {
        // BUG-004: a Multica server hosted under a base path (e.g.
        // https://host/cotter-openclaw) builds URLs like
        // /cotter-openclaw/api/daemon/register, which a plain "/api/" prefix
        // rejects. The "*/api/" wildcard form allows the API subtree under
        // any deployment prefix.
        let validator = AllowlistValidator::new(vec![
            EndpointPattern::host("*").with_path_prefix("*/api/"),
        ]);

        // Root deployment still works.
        assert!(
            validator
                .validate("https://multica.example.com/api/daemon/register", "POST")
                .is_allowed()
        );
        // Base-path deployment is now allowed.
        assert!(
            validator
                .validate(
                    "https://multica.example.com/cotter-openclaw/api/daemon/register",
                    "POST"
                )
                .is_allowed()
        );
        // Paths outside the API subtree stay denied, classified as path denies.
        let result = validator.validate("https://multica.example.com/cotter-openclaw/admin", "GET");
        assert!(!result.is_allowed());
        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::PathNotAllowed { .. }));
        } else {
            panic!("Expected denied");
        }
        // Traversal cannot escape into a non-API path: normalization collapses
        // `/api/..` before matching, leaving no `/api/` segment.
        assert!(
            !validator
                .validate(
                    "https://multica.example.com/cotter-openclaw/api/../admin",
                    "GET"
                )
                .is_allowed()
        );
    }

    #[test]
    fn test_reject_url_with_userinfo() {
        let validator = validator_with_patterns();

        // Attacker uses userinfo to trick the parser: the allowlist sees
        // "api.openai.com" but reqwest would actually connect to "evil.com".
        let result = validator.validate("https://api.openai.com@evil.com/v1/steal", "GET");
        assert!(!result.is_allowed());

        if let super::AllowlistResult::Denied(reason) = result {
            assert!(matches!(reason, DenyReason::InvalidUrl(_)));
        } else {
            panic!("Expected denied due to userinfo");
        }
    }

    #[test]
    fn test_reject_url_with_user_pass() {
        let validator = validator_with_patterns();

        let result = validator.validate("https://user:password@api.openai.com/v1/chat", "GET");
        assert!(!result.is_allowed());
    }
}
