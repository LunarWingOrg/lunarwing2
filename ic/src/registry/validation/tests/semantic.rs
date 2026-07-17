use crate::registry::validation::FindingCode;

use super::{has, validate};

#[test]
fn stdio_requires_explicit_none_auth() {
    for auth in ["", r#", "auth":"dcr""#] {
        let report = validate(&[("mcp-servers", "auth.json", stdio(auth, ""))]);
        assert!(has(&report, FindingCode::InvalidStdioAuth));
    }
}

#[test]
fn command_nul_has_specific_finding() {
    let report = validate(&[(
        "mcp-servers",
        "command.json",
        stdio(r#", "auth":"none""#, r#", "command":"x\u0000y""#),
    )]);

    assert!(has(&report, FindingCode::InvalidStdioCommand));
    assert!(!has(&report, FindingCode::InvalidManifest));
}

#[test]
fn argument_nul_has_specific_finding() {
    let report = validate(&[(
        "mcp-servers",
        "args.json",
        stdio(r#", "auth":"none""#, r#", "args":["x\u0000y"]"#),
    )]);

    assert!(has(&report, FindingCode::InvalidStdioArgs));
}

#[test]
fn invalid_environment_name_has_specific_finding() {
    let report = validate(&[(
        "mcp-servers",
        "env-name.json",
        stdio(r#", "auth":"none""#, r#", "env":{"BAD=NAME":"x"}"#),
    )]);

    assert!(has(&report, FindingCode::InvalidStdioEnv));
}

#[test]
fn environment_nul_has_specific_finding() {
    let report = validate(&[(
        "mcp-servers",
        "env-value.json",
        stdio(r#", "auth":"none""#, r#", "env":{"KEY":"x\u0000y"}"#),
    )]);

    assert!(has(&report, FindingCode::InvalidStdioEnv));
}

#[test]
fn invalid_remote_http_url_uses_typed_validation() {
    let report = validate(&[(
        "mcp-servers",
        "http.json",
        r#"{
            "name":"http","display_name":"HTTP","kind":"mcp_server","description":"test",
            "url":"http://example.com/mcp","auth":"dcr"
        }"#,
    )]);

    assert!(has(&report, FindingCode::InvalidManifest));
}

#[test]
fn valid_http_and_stdio_are_clean() {
    let report = validate(&[
        (
            "mcp-servers",
            "http.json",
            r#"{
                "name":"http","display_name":"HTTP","kind":"mcp_server","description":"test",
                "url":"https://example.com/mcp","auth":"dcr"
            }"#,
        ),
        (
            "mcp-servers",
            "stdio.json",
            stdio(
                r#", "auth":"none""#,
                r#", "args":["-y","server"], "env":{"LOG_LEVEL":"warn"}"#,
            ),
        ),
    ]);

    assert!(
        report.is_clean(),
        "unexpected findings: {:?}",
        report.findings
    );
}

#[test]
fn missing_namespace_directories_are_clean() {
    let directory = tempfile::tempdir().expect("create empty registry");
    let report = crate::registry::validation::validate_registry_dir(directory.path());

    assert!(report.is_clean());
}

fn stdio(auth: &str, transport_fields: &str) -> &'static str {
    Box::leak(
        format!(
            r#"{{
                "name":"stdio","display_name":"stdio","kind":"mcp_server","description":"test",
                "transport":{{"type":"stdio","command":"run"{transport_fields}}}{auth}
            }}"#
        )
        .into_boxed_str(),
    )
}
