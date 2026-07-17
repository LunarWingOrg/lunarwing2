//! Raw-first registry manifest validation.
//!
//! Validation scans source manifests without constructing a [`RegistryCatalog`],
//! so malformed files and duplicate names are reported before typed catalog
//! loading can abort or overwrite an entry.

mod mcp;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::manifest::{ExtensionManifest, ManifestKind};

/// Stable, machine-readable validation categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FindingCode {
    ReadError,
    MalformedJson,
    UnsupportedTransport,
    BothUrlAndTransport,
    NeitherUrlNorTransport,
    InvalidStdioCommand,
    InvalidStdioArgs,
    InvalidStdioEnv,
    InvalidStdioAuth,
    DuplicateName,
    InvalidManifest,
}

impl FindingCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadError => "read_error",
            Self::MalformedJson => "malformed_json",
            Self::UnsupportedTransport => "unsupported_transport",
            Self::BothUrlAndTransport => "both_url_and_transport",
            Self::NeitherUrlNorTransport => "neither_url_nor_transport",
            Self::InvalidStdioCommand => "invalid_stdio_command",
            Self::InvalidStdioArgs => "invalid_stdio_args",
            Self::InvalidStdioEnv => "invalid_stdio_env",
            Self::InvalidStdioAuth => "invalid_stdio_auth",
            Self::DuplicateName => "duplicate_name",
            Self::InvalidManifest => "invalid_manifest",
        }
    }
}

/// A single validation failure for one manifest file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationFinding {
    pub file: String,
    pub kind: ManifestKind,
    pub code: FindingCode,
    pub message: String,
}

/// Aggregated result for a registry source directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub findings: Vec<ValidationFinding>,
}

impl ValidationReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

pub(super) fn finding(
    findings: &mut Vec<ValidationFinding>,
    label: &str,
    kind: ManifestKind,
    code: FindingCode,
    message: impl Into<String>,
) {
    findings.push(ValidationFinding {
        file: label.to_string(),
        kind,
        code,
        message: message.into(),
    });
}

/// Validate all JSON manifests under `tools`, `channels`, and `mcp-servers`.
///
/// Missing namespace directories are valid empty namespaces. Read, parse, raw
/// shape, duplicate-name, and typed semantic failures are aggregated and
/// sorted before this function returns.
#[must_use]
pub fn validate_registry_dir(registry_dir: &Path) -> ValidationReport {
    let mut findings = Vec::new();

    for (kind, namespace) in [
        (ManifestKind::Tool, "tools"),
        (ManifestKind::Channel, "channels"),
        (ManifestKind::McpServer, "mcp-servers"),
    ] {
        validate_namespace(registry_dir, namespace, kind, &mut findings);
    }

    findings.sort_by(|left, right| {
        (&left.file, left.code, &left.message).cmp(&(&right.file, right.code, &right.message))
    });
    ValidationReport { findings }
}

fn validate_namespace(
    registry_dir: &Path,
    namespace: &str,
    kind: ManifestKind,
    findings: &mut Vec<ValidationFinding>,
) {
    let directory = registry_dir.join(namespace);
    if !directory.exists() {
        return;
    }

    let mut paths = match manifest_paths(&directory, namespace, kind, findings) {
        Some(paths) => paths,
        None => return,
    };
    paths.sort();

    let mut names = HashMap::<String, String>::new();
    for path in paths {
        let label = manifest_label(namespace, &path);
        validate_file(&path, &label, kind, &mut names, findings);
    }
}

fn manifest_paths(
    directory: &Path,
    namespace: &str,
    kind: ManifestKind,
    findings: &mut Vec<ValidationFinding>,
) -> Option<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            finding(
                findings,
                &format!("{namespace}/"),
                kind,
                FindingCode::ReadError,
                format!("failed to read directory: {error}"),
            );
            return None;
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file()
                    && path
                        .extension()
                        .is_some_and(|extension| extension == "json")
                {
                    paths.push(path);
                }
            }
            Err(error) => finding(
                findings,
                &format!("{namespace}/"),
                kind,
                FindingCode::ReadError,
                format!("failed to read directory entry: {error}"),
            ),
        }
    }
    Some(paths)
}

fn manifest_label(namespace: &str, path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    format!("{namespace}/{file_name}")
}

fn validate_file(
    path: &Path,
    label: &str,
    kind: ManifestKind,
    names: &mut HashMap<String, String>,
    findings: &mut Vec<ValidationFinding>,
) {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            finding(
                findings,
                label,
                kind,
                FindingCode::ReadError,
                format!("failed to read file: {error}"),
            );
            return;
        }
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(json) => json,
        Err(error) => {
            finding(
                findings,
                label,
                kind,
                FindingCode::MalformedJson,
                format!("invalid JSON: {error}"),
            );
            return;
        }
    };
    let Some(object) = json.as_object() else {
        finding(
            findings,
            label,
            kind,
            FindingCode::InvalidManifest,
            "JSON root must be an object",
        );
        return;
    };

    record_name(object, label, kind, names, findings);
    let raw_status = if kind == ManifestKind::McpServer {
        mcp::validate_raw(object, label, findings)
    } else {
        mcp::RawStatus::VALID
    };
    if !raw_status.typed_compatible {
        return;
    }

    let manifest: ExtensionManifest = match serde_json::from_value(json) {
        Ok(manifest) => manifest,
        Err(error) => {
            finding(
                findings,
                label,
                kind,
                FindingCode::InvalidManifest,
                format!("typed deserialization failed: {error}"),
            );
            return;
        }
    };
    if kind == ManifestKind::McpServer && raw_status.semantic_compatible {
        mcp::validate_typed(&manifest, label, findings);
    }
}

fn record_name(
    object: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    kind: ManifestKind,
    names: &mut HashMap<String, String>,
    findings: &mut Vec<ValidationFinding>,
) {
    let Some(name) = object.get("name").and_then(serde_json::Value::as_str) else {
        return;
    };
    if let Some(first_file) = names.get(name) {
        finding(
            findings,
            label,
            kind,
            FindingCode::DuplicateName,
            format!("duplicate name '{name}'; first declared in {first_file}"),
        );
    } else {
        names.insert(name.to_string(), label.to_string());
    }
}

#[cfg(test)]
mod tests;
