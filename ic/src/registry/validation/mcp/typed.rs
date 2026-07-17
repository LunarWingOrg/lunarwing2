use crate::registry::manifest::{ExtensionManifest, McpManifestTransport};
use crate::registry::validation::{FindingCode, ValidationFinding};
use crate::tools::mcp::config::McpServerConfig;

use super::push;

pub(super) fn validate(
    manifest: &ExtensionManifest,
    label: &str,
    findings: &mut Vec<ValidationFinding>,
) {
    let config = match (&manifest.url, &manifest.transport) {
        (Some(url), None) => McpServerConfig::new(&manifest.name, url),
        (None, Some(McpManifestTransport::Stdio { command, args, env })) => {
            McpServerConfig::new_stdio(&manifest.name, command, args.clone(), env.clone())
        }
        (Some(_), Some(_)) | (None, None) => return,
    };
    if let Err(error) = config.validate() {
        push(
            findings,
            label,
            FindingCode::InvalidManifest,
            error.to_string(),
        );
    }
}
