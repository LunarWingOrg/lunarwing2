//! Generic external runtime descriptors for MCP.
//!
//! The built-in runtime registry is a closed dispatch boundary over the
//! in-tree transports (HTTP, stdio, Unix, WASM, gateway). External runtime
//! descriptors let an operator register an out-of-tree runtime **without
//! adding a core enum variant**: the descriptor names a local adapter process
//! that bridges the foreign runtime to MCP over stdio.
//!
//! Contract:
//! - The descriptor's `command` is spawned like any stdio MCP server; it must
//!   speak MCP (protocol revision `2024-11-05`) over stdin/stdout.
//! - The server entry's opaque `options` JSON is delivered in the
//!   `LUNARWING_MCP_EXTERNAL_OPTIONS` environment variable (bounded).
//! - Descriptors may declare `require_sandbox`, in which case activation
//!   applies at least the mandatory baseline sandbox profile
//!   ([`crate::tools::mcp::sandbox::McpSandboxConfig::default_profile`]) when
//!   the server does not carry an explicit policy.
//!
//! Descriptors are versioned (`descriptor_version`, currently 1) so the
//! contract can evolve without changing the core runtime enums.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// Environment variable that carries the server's options JSON to the adapter.
pub const EXTERNAL_OPTIONS_ENV: &str = "LUNARWING_MCP_EXTERNAL_OPTIONS";
/// Maximum serialized size of the options JSON passed to an adapter.
pub const MAX_EXTERNAL_OPTIONS_BYTES: usize = 16 * 1024;
/// Currently supported descriptor schema version.
pub const DESCRIPTOR_VERSION: u32 = 1;

/// Error returned by descriptor validation or registry operations.
#[derive(Debug, thiserror::Error)]
pub enum DescriptorError {
    #[error("invalid external runtime descriptor: {reason}")]
    Invalid { reason: String },
    #[error("external runtime '{name}' is already registered")]
    Duplicate { name: String },
    #[error("no external runtime descriptor registered for '{name}'")]
    Unknown { name: String },
}

/// Versioned description of an external MCP runtime adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalRuntimeDescriptor {
    /// Descriptor schema version; currently [`DESCRIPTOR_VERSION`].
    pub descriptor_version: u32,
    /// Runtime kind name referenced by `McpTransportConfig::External`
    /// (e.g. "node-compat"). Unique within a registry.
    pub name: String,
    /// Human-readable description (advisory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Adapter executable that bridges the foreign runtime to MCP stdio.
    pub command: String,
    /// Static adapter arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Static adapter environment (secrets must come from env_secrets on the
    /// server entry, never from this static map).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// When true, activation applies at least the mandatory baseline sandbox
    /// profile if the server entry has no explicit policy.
    #[serde(default)]
    pub require_sandbox: bool,
    /// Informational capability names the runtime supports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl ExternalRuntimeDescriptor {
    /// Structural validation; platform enforcement happens at spawn time.
    pub fn validate(&self) -> Result<(), DescriptorError> {
        let invalid = |reason: &str| DescriptorError::Invalid {
            reason: reason.to_string(),
        };
        if self.descriptor_version != DESCRIPTOR_VERSION {
            return Err(invalid(&format!(
                "unsupported descriptor_version {} (expected {DESCRIPTOR_VERSION})",
                self.descriptor_version
            )));
        }
        validate_runtime_name(&self.name)?;
        if self.command.is_empty()
            || self.command.contains('\0')
            || self.args.iter().any(|arg| arg.contains('\0'))
        {
            return Err(invalid(
                "command and args cannot be empty or contain NUL bytes",
            ));
        }
        for (name, value) in &self.env {
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err(invalid(&format!("invalid environment variable '{name}'")));
            }
        }
        if let Some(description) = &self.description
            && (description.len() > 512 || description.chars().any(char::is_control))
        {
            return Err(invalid("description must be <= 512 chars without controls"));
        }
        for capability in &self.capabilities {
            if capability.is_empty()
                || capability.len() > 128
                || capability.bytes().any(|b| b < 0x21 || b == 0x7f)
            {
                return Err(invalid(&format!("invalid capability name '{capability}'")));
            }
        }
        Ok(())
    }
}

/// Validate a runtime kind name referenced from server configuration.
pub fn validate_runtime_name(name: &str) -> Result<(), DescriptorError> {
    let invalid = || DescriptorError::Invalid {
        reason: format!("invalid external runtime name '{name}'"),
    };
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid());
    }
    Ok(())
}

/// Registry of external runtime descriptors, keyed by runtime name.
#[derive(Debug, Default)]
pub struct ExternalRuntimeRegistry {
    runtimes: HashMap<String, ExternalRuntimeDescriptor>,
}

impl ExternalRuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate and register a descriptor. Duplicates are rejected.
    pub fn register(
        &mut self,
        descriptor: ExternalRuntimeDescriptor,
    ) -> Result<(), DescriptorError> {
        descriptor.validate()?;
        if self.runtimes.contains_key(&descriptor.name) {
            return Err(DescriptorError::Duplicate {
                name: descriptor.name.clone(),
            });
        }
        self.runtimes.insert(descriptor.name.clone(), descriptor);
        Ok(())
    }

    /// Look up a descriptor by runtime name.
    pub fn get(&self, name: &str) -> Option<&ExternalRuntimeDescriptor> {
        self.runtimes.get(name)
    }

    pub fn len(&self) -> usize {
        self.runtimes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.runtimes.is_empty()
    }

    /// Registered runtime names (sorted for deterministic output).
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.runtimes.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ExternalRuntimeDescriptor {
        ExternalRuntimeDescriptor {
            descriptor_version: DESCRIPTOR_VERSION,
            name: "node-compat".to_string(),
            description: Some("Bridges legacy node servers".to_string()),
            command: "/usr/local/bin/lw-mcp-node-adapter".to_string(),
            args: vec!["--stdio".to_string()],
            env: BTreeMap::from([("ADAPTER_LOG".to_string(), "warn".to_string())]),
            require_sandbox: true,
            capabilities: vec!["tools".to_string()],
        }
    }

    #[test]
    fn descriptor_validates_and_roundtrips() {
        let descriptor = descriptor();
        descriptor.validate().expect("valid descriptor");
        let json = serde_json::to_string(&descriptor).expect("serialize");
        let parsed: ExternalRuntimeDescriptor = serde_json::from_str(&json).expect("deserialize");
        parsed.validate().expect("roundtrip validates");
        assert_eq!(parsed.name, "node-compat");
        assert!(parsed.require_sandbox);
    }

    #[test]
    fn descriptor_rejects_bad_shapes() {
        let mut bad = descriptor();
        bad.descriptor_version = 99;
        assert!(bad.validate().is_err());

        let mut bad = descriptor();
        bad.name = "has space".to_string();
        assert!(bad.validate().is_err());

        let mut bad = descriptor();
        bad.command = String::new();
        assert!(bad.validate().is_err());

        let mut bad = descriptor();
        bad.env.insert("BAD=NAME".to_string(), "x".to_string());
        assert!(bad.validate().is_err());
    }

    #[test]
    fn registry_registers_and_rejects_duplicates() {
        let mut registry = ExternalRuntimeRegistry::new();
        assert!(registry.is_empty());
        registry.register(descriptor()).expect("register");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.names(), vec!["node-compat"]);
        assert!(registry.get("node-compat").is_some());

        let error = registry.register(descriptor()).expect_err("duplicate");
        assert!(matches!(error, DescriptorError::Duplicate { .. }));
    }

    #[test]
    fn runtime_names_accept_safe_charsets() {
        for name in ["node-compat", "python_v2", "local.adapter"] {
            validate_runtime_name(name).expect("valid name");
        }
        for name in ["", "has space", "slash/escape", "semi;colon"] {
            validate_runtime_name(name).expect_err("invalid name");
        }
    }
}
