use std::fs;

use crate::registry::validation::{FindingCode, ValidationReport, validate_registry_dir};

mod raw;
mod semantic;

fn registry(fixtures: &[(&str, &str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("create temporary registry");
    for (namespace, file_name, content) in fixtures {
        let namespace_directory = directory.path().join(namespace);
        fs::create_dir_all(&namespace_directory).expect("create registry namespace");
        fs::write(namespace_directory.join(file_name), content).expect("write manifest fixture");
    }
    directory
}

fn validate(fixtures: &[(&str, &str, &str)]) -> ValidationReport {
    let directory = registry(fixtures);
    validate_registry_dir(directory.path())
}

fn has(report: &ValidationReport, code: FindingCode) -> bool {
    report.findings.iter().any(|finding| finding.code == code)
}

const VALID_TOOL: &str = r#"{
    "name":"shared","display_name":"Shared","kind":"tool","version":"1.0.0",
    "description":"tool","source":{"dir":"x","capabilities":"x.json","crate_name":"x"}
}"#;

const INVALID_TYPED_TOOL_WITH_NAME: &str =
    r#"{"name":"shared","display_name":"Broken","kind":"tool"}"#;
