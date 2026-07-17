mod stdio;
mod typed;

use crate::registry::manifest::ExtensionManifest;
use crate::registry::manifest::ManifestKind;
use crate::registry::validation::{FindingCode, ValidationFinding, finding};

#[derive(Debug, Clone, Copy)]
pub(super) struct RawStatus {
    pub typed_compatible: bool,
    pub semantic_compatible: bool,
}

impl RawStatus {
    pub const VALID: Self = Self {
        typed_compatible: true,
        semantic_compatible: true,
    };

    pub(super) const fn merge(self, other: Self) -> Self {
        Self {
            typed_compatible: self.typed_compatible && other.typed_compatible,
            semantic_compatible: self.semantic_compatible && other.semantic_compatible,
        }
    }
}

pub(super) fn validate_raw(
    object: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) -> RawStatus {
    let has_url = object.get("url").is_some_and(|value| !value.is_null());
    let has_transport = object
        .get("transport")
        .is_some_and(|value| !value.is_null());
    let mut status = match (has_url, has_transport) {
        (true, true) => {
            push(
                findings,
                label,
                FindingCode::BothUrlAndTransport,
                "declares both 'url' and 'transport'",
            );
            semantic_failure()
        }
        (false, false) => {
            push(
                findings,
                label,
                FindingCode::NeitherUrlNorTransport,
                "declares neither 'url' nor 'transport'",
            );
            semantic_failure()
        }
        (true, false) | (false, true) => RawStatus::VALID,
    };

    if has_transport {
        status = status.merge(validate_transport(object, label, findings));
    }
    status
}

pub(super) fn validate_typed(
    manifest: &ExtensionManifest,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) {
    typed::validate(manifest, label, findings);
}

fn validate_transport(
    object: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) -> RawStatus {
    let Some(transport) = object
        .get("transport")
        .and_then(serde_json::Value::as_object)
    else {
        return invalid_shape(
            findings,
            label,
            FindingCode::InvalidManifest,
            "'transport' must be an object",
        );
    };
    let transport_type = transport.get("type").and_then(serde_json::Value::as_str);
    if transport_type != Some("stdio") {
        return invalid_shape(
            findings,
            label,
            FindingCode::UnsupportedTransport,
            format!(
                "unsupported transport type '{}'",
                transport_type.unwrap_or("<missing>")
            ),
        );
    }
    stdio::validate(transport, object, label, findings)
}

pub(super) fn invalid_shape(
    findings: &mut Vec<ValidationFinding>,
    label: &str,
    code: FindingCode,
    message: impl Into<String>,
) -> RawStatus {
    push(findings, label, code, message);
    RawStatus {
        typed_compatible: false,
        semantic_compatible: false,
    }
}

pub(super) fn invalid_semantic(
    findings: &mut Vec<ValidationFinding>,
    label: &str,
    code: FindingCode,
    message: impl Into<String>,
) -> RawStatus {
    push(findings, label, code, message);
    semantic_failure()
}

const fn semantic_failure() -> RawStatus {
    RawStatus {
        typed_compatible: true,
        semantic_compatible: false,
    }
}

pub(super) fn push(
    findings: &mut Vec<ValidationFinding>,
    label: &str,
    code: FindingCode,
    message: impl Into<String>,
) {
    finding(findings, label, ManifestKind::McpServer, code, message);
}
