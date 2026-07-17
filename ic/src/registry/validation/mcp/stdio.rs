use crate::registry::validation::{FindingCode, ValidationFinding};

use super::{RawStatus, invalid_semantic, invalid_shape};

pub(super) fn validate(
    transport: &serde_json::Map<String, serde_json::Value>,
    manifest: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) -> RawStatus {
    validate_command(transport, label, findings)
        .merge(validate_args(transport, label, findings))
        .merge(validate_env(transport, label, findings))
        .merge(validate_auth(manifest, label, findings))
}

fn validate_command(
    transport: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) -> RawStatus {
    match transport.get("command") {
        Some(serde_json::Value::String(command)) if command.is_empty() => invalid_semantic(
            findings,
            label,
            FindingCode::InvalidStdioCommand,
            "command is empty",
        ),
        Some(serde_json::Value::String(command)) if command.contains('\0') => invalid_semantic(
            findings,
            label,
            FindingCode::InvalidStdioCommand,
            "command contains a NUL byte",
        ),
        Some(serde_json::Value::String(_)) => RawStatus::VALID,
        Some(_) => invalid_shape(
            findings,
            label,
            FindingCode::InvalidStdioCommand,
            "command must be a string",
        ),
        None => invalid_shape(
            findings,
            label,
            FindingCode::InvalidStdioCommand,
            "missing 'command'",
        ),
    }
}

fn validate_args(
    transport: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) -> RawStatus {
    let Some(value) = transport.get("args") else {
        return RawStatus::VALID;
    };
    let Some(arguments) = value.as_array() else {
        return invalid_shape(
            findings,
            label,
            FindingCode::InvalidStdioArgs,
            "'args' must be an array",
        );
    };

    let mut status = RawStatus::VALID;
    for argument in arguments {
        status = status.merge(match argument.as_str() {
            Some(argument) if argument.contains('\0') => invalid_semantic(
                findings,
                label,
                FindingCode::InvalidStdioArgs,
                "argument contains a NUL byte",
            ),
            Some(_) => RawStatus::VALID,
            None => invalid_shape(
                findings,
                label,
                FindingCode::InvalidStdioArgs,
                "all arguments must be strings",
            ),
        });
    }
    status
}

fn validate_env(
    transport: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) -> RawStatus {
    let Some(value) = transport.get("env") else {
        return RawStatus::VALID;
    };
    let Some(environment) = value.as_object() else {
        return invalid_shape(
            findings,
            label,
            FindingCode::InvalidStdioEnv,
            "'env' must be an object",
        );
    };

    let mut status = RawStatus::VALID;
    for (name, value) in environment {
        if name.is_empty() || name.contains(['=', '\0']) {
            status = status.merge(invalid_semantic(
                findings,
                label,
                FindingCode::InvalidStdioEnv,
                format!("invalid environment variable name '{name}'"),
            ));
        }
        status = status.merge(match value.as_str() {
            Some(value) if value.contains('\0') => invalid_semantic(
                findings,
                label,
                FindingCode::InvalidStdioEnv,
                format!("environment value for '{name}' contains a NUL byte"),
            ),
            Some(_) => RawStatus::VALID,
            None => invalid_shape(
                findings,
                label,
                FindingCode::InvalidStdioEnv,
                format!("environment value for '{name}' must be a string"),
            ),
        });
    }
    status
}

fn validate_auth(
    manifest: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) -> RawStatus {
    if manifest.get("auth").and_then(serde_json::Value::as_str) == Some("none") {
        return RawStatus::VALID;
    }
    invalid_semantic(
        findings,
        label,
        FindingCode::InvalidStdioAuth,
        "stdio MCP manifest must declare \"auth\": \"none\"",
    )
}
