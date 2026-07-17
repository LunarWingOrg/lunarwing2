use crate::registry::validation::FindingCode;

use super::{INVALID_TYPED_TOOL_WITH_NAME, VALID_TOOL, has, validate};

#[test]
fn malformed_json_is_reported_without_stopping_the_scan() {
    let report = validate(&[
        ("tools", "broken.json", "{not json"),
        ("mcp-servers", "missing.json", mcp("")),
    ]);

    assert!(has(&report, FindingCode::MalformedJson));
    assert!(has(&report, FindingCode::NeitherUrlNorTransport));
}

#[test]
fn unsupported_transport_has_one_specific_finding() {
    let report = validate(&[(
        "mcp-servers",
        "ws.json",
        mcp(r#", "transport":{"type":"websocket"}"#),
    )]);

    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].code, FindingCode::UnsupportedTransport);
}

#[test]
fn url_and_transport_are_mutually_exclusive() {
    let report = validate(&[(
        "mcp-servers",
        "both.json",
        mcp(
            r#", "url":"https://example.com", "transport":{"type":"stdio","command":"x"}, "auth":"none""#,
        ),
    )]);

    assert!(has(&report, FindingCode::BothUrlAndTransport));
}

#[test]
fn null_url_and_transport_count_as_absent() {
    let report = validate(&[(
        "mcp-servers",
        "null.json",
        mcp(r#", "url":null, "transport":null"#),
    )]);

    assert!(has(&report, FindingCode::NeitherUrlNorTransport));
}

#[test]
fn stdio_shape_errors_are_specific() {
    let report = validate(&[(
        "mcp-servers",
        "shape.json",
        mcp(r#", "transport":{"type":"stdio","command":7,"args":"bad","env":[]}"#),
    )]);

    assert!(has(&report, FindingCode::InvalidStdioCommand));
    assert!(has(&report, FindingCode::InvalidStdioArgs));
    assert!(has(&report, FindingCode::InvalidStdioEnv));
}

#[test]
fn non_string_argument_is_reported() {
    let report = validate(&[(
        "mcp-servers",
        "args.json",
        mcp(r#", "transport":{"type":"stdio","command":"x","args":[1]}, "auth":"none""#),
    )]);

    assert!(has(&report, FindingCode::InvalidStdioArgs));
}

#[test]
fn duplicate_raw_name_survives_typed_failure() {
    let report = validate(&[
        ("tools", "a.json", VALID_TOOL),
        ("tools", "b.json", INVALID_TYPED_TOOL_WITH_NAME),
    ]);

    assert!(has(&report, FindingCode::DuplicateName));
    assert!(has(&report, FindingCode::InvalidManifest));
    assert!(report.findings.iter().any(|finding| {
        finding.code == FindingCode::DuplicateName
            && finding.message.contains("tools/a.json")
            && finding.file == "tools/b.json"
    }));
}

#[test]
fn cross_namespace_names_are_allowed() {
    let report = validate(&[
        ("tools", "shared.json", VALID_TOOL),
        ("channels", "shared.json", valid_channel()),
    ]);

    assert!(!has(&report, FindingCode::DuplicateName));
}

#[test]
fn findings_have_deterministic_order() {
    let report = validate(&[
        ("tools", "z.json", "{bad"),
        ("channels", "a.json", "{bad"),
        ("mcp-servers", "m.json", mcp("")),
    ]);
    let mut sorted = report.findings.clone();
    sorted.sort_by(|left, right| {
        (&left.file, left.code, &left.message).cmp(&(&right.file, right.code, &right.message))
    });

    assert_eq!(report.findings, sorted);
}

fn mcp(fields: &str) -> &'static str {
    Box::leak(
        format!(
            r#"{{"name":"mcp","display_name":"MCP","kind":"mcp_server","description":"test"{fields}}}"#
        )
        .into_boxed_str(),
    )
}

fn valid_channel() -> &'static str {
    r#"{
        "name":"shared","display_name":"Shared","kind":"channel","version":"1.0.0",
        "description":"channel","source":{"dir":"x","capabilities":"x.json","crate_name":"x"}
    }"#
}
