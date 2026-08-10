//! Unix-only, short-lived, secret-minimizing DarkIRC config/adoption helper.

#![cfg(unix)]

use std::fs;
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use cap_fs_ext::{
    DirExt, FollowSymlinks, OpenOptionsExt as CapOpenOptionsExt, OpenOptionsFollowExt,
    OpenOptionsSyncExt,
};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use lunarwing::darkirc_key_manager::{
    CompatibilityAttestation, CompleteExchange, ContactSummary, DEFAULT_OFFER_TTL_SECONDS,
    ExchangeStatus, GeneratedExchangeMaterial, KeyManagerError, MAX_BASELINE_BYTES,
    MAX_MIGRATION_BYTES, MAX_PENDING_BYTES, PrepareExchangeOperation, PublicFingerprint,
    RespondExchangeOperation, Result, TenantPaths, adopt_contacts, cancel_exchange,
    complete_exchange, doctor_contacts, export_migration, import_migration, inspect_contacts,
    inspect_exchanges, migration_ready, prepare_exchange_with_generator, read_ledger, recover,
    respond_exchange_with_generator, stage_migration, update_baseline, validate_migration,
    validate_migration_unbound,
};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rand::RngCore;
use secrecy::SecretString;
use serde::Serialize;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

const RELEASE_DAEMON_NAME: &str = "darkirc";
const RELEASE_HELPER_NAME: &str = "lunarwing-darkirc-key-helper";
const GENERATOR_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_GENERATOR_OUTPUT_BYTES: usize = 8 * 1024;
const GENERATOR_POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Debug, Default)]
struct Options {
    tenant: Option<String>,
    scope_id: Option<String>,
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
    contact: Option<String>,
    generator_profile: Option<String>,
    binary_sha256: Option<String>,
    binary_path: Option<String>,
    helper_sha256: Option<String>,
    release_id: Option<String>,
    key_format: Option<String>,
    source_revision: Option<String>,
    unbound_validation: bool,
    yes: bool,
    out_path: Option<String>,
    in_path: Option<String>,
    expect_peer_fingerprint: Option<String>,
    expires_seconds: Option<i64>,
    defer_apply: bool,
    json: bool,
    exchange_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    contacts: Vec<ContactSummary>,
}

#[derive(Debug, Serialize)]
struct MigrationStatus<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_id: Option<&'a str>,
}

pub(crate) fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or(KeyManagerError::InvalidTenant)?;
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tenant" => options.tenant = Some(next_value(&mut args)?),
            "--scope-id" => options.scope_id = Some(next_value(&mut args)?),
            "--expected-uid" => options.expected_uid = Some(parse_u32(&next_value(&mut args)?)?),
            "--expected-gid" => options.expected_gid = Some(parse_u32(&next_value(&mut args)?)?),
            "--contact" => options.contact = Some(next_value(&mut args)?),
            "--generator-profile" => options.generator_profile = Some(next_value(&mut args)?),
            "--binary-sha256" => options.binary_sha256 = Some(next_value(&mut args)?),
            "--binary-path" => options.binary_path = Some(next_value(&mut args)?),
            "--helper-sha256" => options.helper_sha256 = Some(next_value(&mut args)?),
            "--release-id" => options.release_id = Some(next_value(&mut args)?),
            "--key-format" => options.key_format = Some(next_value(&mut args)?),
            "--source-revision" => options.source_revision = Some(next_value(&mut args)?),
            "--unbound" => options.unbound_validation = true,
            "--yes" => options.yes = true,
            "--json" => options.json = true,
            "--out" => options.out_path = Some(next_value(&mut args)?),
            "--in" => options.in_path = Some(next_value(&mut args)?),
            "--expect-peer-fingerprint" => {
                options.expect_peer_fingerprint = Some(next_value(&mut args)?)
            }
            "--expires" => options.expires_seconds = Some(parse_i64(&next_value(&mut args)?)?),
            "--defer-apply" => options.defer_apply = true,
            "--exchange-id" => options.exchange_id = Some(next_value(&mut args)?),
            _ => return Err(KeyManagerError::InvalidTenant),
        }
    }

    let validated = if command == "validate-migration" && options.unbound_validation {
        None
    } else {
        Some(compatibility_attestation(&options)?)
    };
    let attestation = validated.as_ref().map(|value| &value.attestation);

    if command == "self-test-generator" {
        let keypair = generate_keypair(
            validated
                .as_ref()
                .ok_or(KeyManagerError::InvalidCompatibility)?,
        )?;
        drop(keypair);
        return print_json(&MigrationStatus {
            status: "generator-compatible",
            scope_id: None,
        });
    }

    if command == "validate-migration" {
        let manifest = read_migration_input(io::stdin())?;
        let validation = if options.unbound_validation {
            validate_migration_unbound(&manifest)?
        } else {
            validate_migration(
                &manifest,
                attestation.ok_or(KeyManagerError::InvalidCompatibility)?,
            )?
        };
        return print_json(&MigrationStatus {
            status: "valid",
            scope_id: Some(&validation.scope_id),
        });
    }

    let tenant = options
        .tenant
        .as_deref()
        .ok_or(KeyManagerError::InvalidTenant)?;
    let expected_uid = options.expected_uid.ok_or(KeyManagerError::InvalidTenant)?;
    let expected_gid = options.expected_gid.ok_or(KeyManagerError::InvalidTenant)?;
    let paths = TenantPaths::for_tenant(tenant, Some(expected_uid), Some(expected_gid))?;
    match command.as_str() {
        "rewrite-baseline" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            let baseline = read_baseline_input(io::stdin())?;
            print_json(&update_baseline(&paths, &scope, &baseline)?)
        }
        "export-migration" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            export_migration(
                &paths,
                &scope,
                attestation.ok_or(KeyManagerError::InvalidCompatibility)?,
            )?;
            print_json(&MigrationStatus {
                status: "exported",
                scope_id: None,
            })
        }
        "stage-migration" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            let manifest = read_migration_input(io::stdin())?;
            stage_migration(
                &paths,
                &scope,
                &manifest,
                attestation.ok_or(KeyManagerError::InvalidCompatibility)?,
            )?;
            print_json(&MigrationStatus {
                status: "staged",
                scope_id: None,
            })
        }
        "import-migration" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            import_migration(
                &paths,
                &scope,
                attestation.ok_or(KeyManagerError::InvalidCompatibility)?,
            )?;
            print_json(&MigrationStatus {
                status: "imported",
                scope_id: None,
            })
        }
        "migration-ready" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            migration_ready(&paths, &scope)?;
            print_json(&MigrationStatus {
                status: "ready",
                scope_id: None,
            })
        }
        "list" => print_json(&StatusOutput {
            contacts: inspect_contacts(&paths, options.scope_id.as_deref())?,
        }),
        "doctor" => print_json(&doctor_contacts(&paths, options.scope_id.as_deref())?),
        "status" => {
            let contacts = inspect_contacts(&paths, options.scope_id.as_deref())?;
            let contact = options.contact.ok_or(KeyManagerError::InvalidContact)?;
            print_json(&select_status_contact(contacts, &contact)?)
        }
        "adopt" => {
            if !options.yes {
                return Err(KeyManagerError::InvalidContact);
            }
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            print_json(&adopt_contacts(&paths, &scope)?)
        }
        "ledger" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            print_json(&read_ledger(&paths, &scope)?)
        }
        "recover" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            recover(&paths, &scope)?;
            print_json(&StatusOutput {
                contacts: Vec::new(),
            })
        }
        "prepare" => {
            let scope = options
                .scope_id
                .take()
                .ok_or(KeyManagerError::InvalidScope)?;
            let contact = options
                .contact
                .take()
                .ok_or(KeyManagerError::InvalidContact)?;
            let attested = attestation.ok_or(KeyManagerError::InvalidCompatibility)?;
            validate_artifact_output(&options)?;
            let now = unix_now()?;
            let ttl = validated_ttl(options.expires_seconds)?;
            let expires_at = exchange_deadline(now, ttl)?;
            let result = prepare_exchange_with_generator(
                &paths,
                &scope,
                &PrepareExchangeOperation {
                    contact_name: &contact,
                    intended_peer_contact_id: None,
                    intended_peer_fingerprint: None,
                    label: None,
                    previous_fingerprint: None,
                    generation: 1,
                    created_at: now,
                    expires_at,
                    now,
                    attestation: attested,
                },
                || {
                    generated_exchange_material(
                        &options,
                        validated
                            .as_ref()
                            .ok_or(KeyManagerError::InvalidCompatibility)?,
                    )
                },
            )?;
            write_committed_artifact(&options, &result.public_artifact, &result)
        }
        "respond" => {
            let scope = options
                .scope_id
                .take()
                .ok_or(KeyManagerError::InvalidScope)?;
            let contact = options
                .contact
                .take()
                .ok_or(KeyManagerError::InvalidContact)?;
            let expected_fp = options
                .expect_peer_fingerprint
                .as_deref()
                .ok_or(KeyManagerError::FingerprintMismatch)?;
            let expected_fp = PublicFingerprint::parse(expected_fp)?;
            validate_artifact_output(&options)?;
            let offer_bytes = read_artifact_input(&options)?;
            let now = unix_now()?;
            let ttl = validated_ttl(options.expires_seconds)?;
            // The response has its own bounded lifetime. Do not shorten it to the
            // initiator's remaining lifetime: delayed delivery may leave little
            // overlap even though the initiator offer is still valid now.
            let expires_at = exchange_deadline(now, ttl)?;
            let result = respond_exchange_with_generator(
                &paths,
                &scope,
                &offer_bytes,
                &RespondExchangeOperation {
                    contact_name: &contact,
                    expected_initiator_fingerprint: &expected_fp,
                    label: None,
                    created_at: now,
                    expires_at,
                    now,
                    attestation: attestation.ok_or(KeyManagerError::InvalidCompatibility)?,
                },
                || {
                    generated_exchange_material(
                        &options,
                        validated
                            .as_ref()
                            .ok_or(KeyManagerError::InvalidCompatibility)?,
                    )
                },
            )?;
            write_committed_artifact(&options, &result.public_artifact, &result)
        }
        "complete" => {
            if !options.defer_apply {
                return Err(KeyManagerError::InvalidExchangeState);
            }
            let scope = options
                .scope_id
                .take()
                .ok_or(KeyManagerError::InvalidScope)?;
            let contact = options
                .contact
                .take()
                .ok_or(KeyManagerError::InvalidContact)?;
            let exchange_id = options
                .exchange_id
                .take()
                .ok_or(KeyManagerError::InvalidExchangeArtifact)?;
            let expected_fp = options
                .expect_peer_fingerprint
                .as_deref()
                .ok_or(KeyManagerError::FingerprintMismatch)?;
            let expected_fp = PublicFingerprint::parse(expected_fp)?;
            let peer_bytes = read_artifact_input(&options)?;
            let request = CompleteExchange {
                exchange_id: &exchange_id,
                contact_name: &contact,
                expected_peer_fingerprint: &expected_fp,
                defer_apply: options.defer_apply,
                now: unix_now()?,
                attestation: attestation.ok_or(KeyManagerError::InvalidCompatibility)?,
            };
            let status = complete_exchange(&paths, &scope, &peer_bytes, &request)?;
            print_json(&status)
        }
        "cancel" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            let exchange_id = options
                .exchange_id
                .as_deref()
                .ok_or(KeyManagerError::InvalidExchangeArtifact)?;
            let status = cancel_exchange(&paths, &scope, exchange_id)?;
            print_json(&status)
        }
        "exchanges" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            let status: Vec<ExchangeStatus> = inspect_exchanges(&paths, &scope)?;
            print_json(&status)
        }
        _ => Err(KeyManagerError::InvalidTenant),
    }
}

#[derive(Debug)]
struct ValidatedCompatibility {
    attestation: CompatibilityAttestation,
    daemon: fs::File,
}

fn compatibility_attestation(options: &Options) -> Result<ValidatedCompatibility> {
    let attestation = CompatibilityAttestation::new(
        options
            .generator_profile
            .as_deref()
            .ok_or(KeyManagerError::InvalidCompatibility)?,
        options
            .binary_sha256
            .as_deref()
            .ok_or(KeyManagerError::InvalidCompatibility)?,
        options
            .key_format
            .as_deref()
            .ok_or(KeyManagerError::InvalidCompatibility)?,
        options
            .source_revision
            .as_deref()
            .ok_or(KeyManagerError::InvalidCompatibility)?,
    )?;
    let binary_path = options
        .binary_path
        .as_deref()
        .ok_or(KeyManagerError::InvalidCompatibility)?;
    let binary_path = Path::new(binary_path);
    let release_dir = binary_path
        .parent()
        .ok_or(KeyManagerError::InvalidCompatibility)?;
    let release_id = options
        .release_id
        .as_deref()
        .filter(|value| valid_release_id(value))
        .ok_or(KeyManagerError::InvalidCompatibility)?;
    if binary_path.file_name().and_then(|value| value.to_str()) != Some(RELEASE_DAEMON_NAME)
        || release_dir.file_name().and_then(|value| value.to_str()) != Some(release_id)
    {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    validate_attested_binary_parents(binary_path)?;
    validate_helper_binding(release_dir, options)?;
    let metadata = fs::symlink_metadata(binary_path).map_err(KeyManagerError::Io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    if metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o111 == 0
        || !attested_binary_mode(metadata.permissions().mode())
    {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(binary_path)
        .map_err(KeyManagerError::Io)?;
    let opened = file.metadata().map_err(KeyManagerError::Io)?;
    if opened.uid() != 0
        || opened.nlink() != 1
        || opened.permissions().mode() & 0o111 == 0
        || !attested_binary_mode(opened.permissions().mode())
        || opened.dev() != metadata.dev()
        || opened.ino() != metadata.ino()
    {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(KeyManagerError::Io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let measured = format!("sha256:{:x}", hasher.finalize());
    if measured != attestation.binary_sha256() {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    Ok(ValidatedCompatibility {
        attestation,
        daemon: file,
    })
}

fn valid_release_id(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() == 4
        && [40, 16, 16, 16]
            .into_iter()
            .zip(parts)
            .all(|(length, part)| {
                part.len() == length
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
}

fn validate_helper_binding(release_dir: &Path, options: &Options) -> Result<()> {
    let helper_path = release_dir.join(RELEASE_HELPER_NAME);
    let current_exe = std::env::current_exe().map_err(KeyManagerError::Io)?;
    if current_exe != helper_path {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    let expected = options
        .helper_sha256
        .as_deref()
        .ok_or(KeyManagerError::InvalidCompatibility)?;
    let mut helper = open_validated_release_executable(&helper_path)?;
    verify_sha256(&mut helper, expected)
}

fn open_validated_release_executable(path: &Path) -> Result<fs::File> {
    let metadata = fs::symlink_metadata(path).map_err(KeyManagerError::Io)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || !attested_binary_mode(metadata.permissions().mode())
    {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(KeyManagerError::Io)?;
    let opened = file.metadata().map_err(KeyManagerError::Io)?;
    if opened.dev() != metadata.dev()
        || opened.ino() != metadata.ino()
        || opened.uid() != 0
        || opened.nlink() != 1
        || !attested_binary_mode(opened.permissions().mode())
    {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    Ok(file)
}

fn sha256_file(file: &mut fs::File) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(KeyManagerError::Io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn verify_sha256(file: &mut fs::File, expected: &str) -> Result<()> {
    if sha256_file(file)? != expected {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    Ok(())
}

fn validate_attested_binary_parents(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or(KeyManagerError::InvalidCompatibility)?;
    let mut current = Path::new("/").to_path_buf();
    for component in parent.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(part) => current.push(part),
            Component::Prefix(_) | Component::ParentDir => {
                return Err(KeyManagerError::InvalidCompatibility);
            }
        }
        let metadata = fs::symlink_metadata(&current).map_err(KeyManagerError::Io)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(KeyManagerError::InvalidCompatibility);
        }
    }
    Ok(())
}

fn attested_binary_mode(mode: u32) -> bool {
    mode & 0o7022 == 0 && mode & 0o111 != 0
}

fn next_value(args: &mut impl Iterator<Item = String>) -> Result<String> {
    args.next().ok_or(KeyManagerError::InvalidTenant)
}

fn parse_u32(value: &str) -> Result<u32> {
    value.parse().map_err(|_| KeyManagerError::InvalidTenant)
}

fn parse_i64(value: &str) -> Result<i64> {
    value.parse().map_err(|_| KeyManagerError::InvalidTenant)
}

fn unix_now() -> Result<i64> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| KeyManagerError::InvalidExchangeArtifact)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| KeyManagerError::InvalidExchangeArtifact)
}

fn validated_ttl(value: Option<i64>) -> Result<i64> {
    let ttl = value.unwrap_or(DEFAULT_OFFER_TTL_SECONDS);
    if !(1..=DEFAULT_OFFER_TTL_SECONDS).contains(&ttl) {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(ttl)
}

fn exchange_deadline(now: i64, ttl: i64) -> Result<i64> {
    now.checked_add(ttl)
        .ok_or(KeyManagerError::InvalidExchangeArtifact)
}

fn random_exchange_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

struct KeypairOutput {
    public_key: String,
    secret_key: SecretString,
}

type CapturedGeneratorOutput = (Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>, bool);

struct ArtifactPath {
    parent: Dir,
    name: String,
}

fn generate_keypair(validated: &ValidatedCompatibility) -> Result<KeypairOutput> {
    #[cfg(target_os = "linux")]
    let executable = format!("/proc/self/fd/{}", validated.daemon.as_raw_fd());
    #[cfg(not(target_os = "linux"))]
    let executable = format!("/dev/fd/{}", validated.daemon.as_raw_fd());
    let mut command = Command::new(executable);
    command
        .arg("--gen-chacha-keypair")
        .env_clear()
        .stdin(Stdio::null());
    let (mut stdout, mut stderr, success) =
        capture_generator_output(command, GENERATOR_TIMEOUT, MAX_GENERATOR_OUTPUT_BYTES)?;
    parse_generator_output(&mut stdout, &mut stderr, success)
}

fn capture_generator_output(
    mut command: Command,
    timeout: Duration,
    output_limit: usize,
) -> Result<CapturedGeneratorOutput> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(KeyManagerError::Io)?;
    let Some(stdout) = child.stdout.take() else {
        kill_generator_process_group(&mut child);
        let _ = child.wait();
        return Err(KeyManagerError::InvalidCompatibility);
    };
    let Some(stderr) = child.stderr.take() else {
        kill_generator_process_group(&mut child);
        let _ = child.wait();
        return Err(KeyManagerError::InvalidCompatibility);
    };
    let total = Arc::new(AtomicUsize::new(0));
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_bounded_output_reader(
        stdout,
        Arc::clone(&total),
        Arc::clone(&overflow),
        output_limit,
    );
    let stderr_reader = spawn_bounded_output_reader(
        stderr,
        Arc::clone(&total),
        Arc::clone(&overflow),
        output_limit,
    );
    let started = Instant::now();
    let mut forced_stop = false;
    let status = loop {
        if overflow.load(Ordering::Acquire) || started.elapsed() >= timeout {
            forced_stop = true;
            kill_generator_process_group(&mut child);
            break child.wait();
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Close pipes held by unexpected descendants before joining readers.
                kill_generator_process_group(&mut child);
                break Ok(status);
            }
            Ok(None) => {}
            Err(error) => {
                forced_stop = true;
                kill_generator_process_group(&mut child);
                let _ = child.wait();
                break Err(error);
            }
        }
        thread::sleep(GENERATOR_POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
    };
    let stdout = join_output_reader(stdout_reader);
    let stderr = join_output_reader(stderr_reader);
    let stdout = stdout?;
    let stderr = stderr?;
    let status = status.map_err(KeyManagerError::Io)?;
    if forced_stop || overflow.load(Ordering::Acquire) {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    Ok((stdout, stderr, status.success()))
}

fn kill_generator_process_group(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    let _ = child.kill();
}

fn spawn_bounded_output_reader<R>(
    mut reader: R,
    total: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
    output_limit: usize,
) -> thread::JoinHandle<io::Result<Zeroizing<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Zeroizing::new(Vec::new());
        let mut buffer = Zeroizing::new([0_u8; 4096]);
        loop {
            let count = reader.read(&mut *buffer)?;
            if count == 0 {
                return Ok(output);
            }
            let previous = total
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.saturating_add(count))
                })
                .unwrap_or(usize::MAX);
            if previous.saturating_add(count) > output_limit {
                overflow.store(true, Ordering::Release);
                return Ok(output);
            }
            output.extend_from_slice(&buffer[..count]);
        }
    })
}

fn join_output_reader(
    reader: thread::JoinHandle<io::Result<Zeroizing<Vec<u8>>>>,
) -> Result<Zeroizing<Vec<u8>>> {
    reader
        .join()
        .map_err(|_| KeyManagerError::InvalidCompatibility)?
        .map_err(KeyManagerError::Io)
}

fn generated_exchange_material(
    options: &Options,
    validated: &ValidatedCompatibility,
) -> Result<GeneratedExchangeMaterial> {
    require_unused_artifact_output(options)?;
    let keypair = generate_keypair(validated)?;
    Ok(GeneratedExchangeMaterial {
        offer_id: random_exchange_id(),
        contact_id: random_exchange_id(),
        local_public: keypair.public_key,
        local_private: keypair.secret_key,
    })
}

fn parse_generator_output(
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    success: bool,
) -> Result<KeypairOutput> {
    let parsed = (|| {
        if !success || !stderr.is_empty() {
            return Err(KeyManagerError::InvalidCompatibility);
        }
        let text =
            std::str::from_utf8(stdout).map_err(|_| KeyManagerError::InvalidCompatibility)?;
        let (public_key, secret_key) =
            parse_strict_key_lines(text).or_else(|_| parse_toml_keypair_lines(text))?;
        validate_generated_pair(&public_key, &secret_key)?;
        Ok(KeypairOutput {
            public_key,
            secret_key: SecretString::from(secret_key.as_str().to_owned()),
        })
    })();
    stdout.zeroize();
    stderr.zeroize();
    parsed
}

/// Strict two-line `Public:` / `Secret:` generator output (legacy format).
fn parse_strict_key_lines(text: &str) -> Result<(String, Zeroizing<String>)> {
    let mut public_key = None;
    let mut secret_key: Option<Zeroizing<String>> = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Public:") {
            let value = value.trim();
            if value.is_empty() || public_key.replace(value.to_owned()).is_some() {
                return Err(KeyManagerError::InvalidCompatibility);
            }
        } else if let Some(value) = line.strip_prefix("Secret:") {
            let value = Zeroizing::new(value.trim().to_owned());
            if value.is_empty() || secret_key.replace(value).is_some() {
                return Err(KeyManagerError::InvalidCompatibility);
            }
        } else {
            return Err(KeyManagerError::InvalidCompatibility);
        }
    }
    let public_key = public_key.ok_or(KeyManagerError::InvalidCompatibility)?;
    let secret_key = secret_key.ok_or(KeyManagerError::InvalidCompatibility)?;
    Ok((public_key, secret_key))
}

/// darkirc's TOML-snippet generator output: the pinned daemon prints a config
/// block with `my_dm_chacha_secret = "..."` and a commented-out
/// `#my_dm_chacha_public = "..."` line. The uncommented
/// `dm_chacha_public = "YOUR_CONTACT_PUBLIC_KEY"` placeholder is
/// documentation, not a key, and is ignored.
fn parse_toml_keypair_lines(text: &str) -> Result<(String, Zeroizing<String>)> {
    let mut public_key = None;
    let mut secret_key: Option<Zeroizing<String>> = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_start_matches('#').trim();
        let value = value.trim().trim_matches('"').trim();
        match key {
            "my_dm_chacha_public" => {
                if value.is_empty() || public_key.replace(value.to_owned()).is_some() {
                    return Err(KeyManagerError::InvalidCompatibility);
                }
            }
            "my_dm_chacha_secret" => {
                let value = Zeroizing::new(value.to_owned());
                if value.is_empty() || secret_key.replace(value).is_some() {
                    return Err(KeyManagerError::InvalidCompatibility);
                }
            }
            _ => {}
        }
    }
    let public_key = public_key.ok_or(KeyManagerError::InvalidCompatibility)?;
    let secret_key = secret_key.ok_or(KeyManagerError::InvalidCompatibility)?;
    Ok((public_key, secret_key))
}

fn decode_generator_key(value: &str) -> Result<Zeroizing<[u8; 32]>> {
    let decoded = Zeroizing::new(
        bs58::decode(value)
            .into_vec()
            .map_err(|_| KeyManagerError::InvalidCompatibility)?,
    );
    let canonical = Zeroizing::new(bs58::encode(decoded.as_slice()).into_string());
    if decoded.len() != 32 || decoded.iter().all(|byte| *byte == 0) || canonical.as_str() != value {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    key.copy_from_slice(&decoded);
    Ok(key)
}

fn validate_generated_pair(public: &str, secret: &str) -> Result<()> {
    let public_bytes = decode_generator_key(public)?;
    let secret_bytes = decode_generator_key(secret)?;
    let secret_key = StaticSecret::from(*secret_bytes);
    let derived = X25519PublicKey::from(&secret_key);
    if derived.as_bytes() != public_bytes.as_ref() {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    Ok(())
}

fn read_artifact_input(options: &Options) -> Result<Zeroizing<Vec<u8>>> {
    match options.in_path.as_deref() {
        Some("-") | None => read_bounded_artifact(io::stdin()),
        Some(path) => {
            validate_relative_path(path)?;
            let artifact = resolve_artifact_path(Path::new(path))?;
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let file = artifact
                .parent
                .open_with(&artifact.name, &options)
                .map_err(KeyManagerError::Io)?
                .into_std();
            let metadata = file.metadata().map_err(KeyManagerError::Io)?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(KeyManagerError::UnsafePath);
            }
            read_bounded_artifact(file)
        }
    }
}

fn read_bounded_artifact(reader: impl Read) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_PENDING_BYTES + 1));
    reader
        .take((MAX_PENDING_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(KeyManagerError::Io)?;
    if bytes.len() > MAX_PENDING_BYTES {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    }
    Ok(bytes)
}

fn validate_relative_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(KeyManagerError::UnsafePath);
    }
    if path
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == "lunarwing")
    {
        return Err(KeyManagerError::UnsafePath);
    }
    Ok(())
}

fn resolve_artifact_path(path: &Path) -> Result<ArtifactPath> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => components.push(part),
            Component::Prefix(_) | Component::ParentDir => {
                return Err(KeyManagerError::UnsafePath);
            }
        }
    }
    let name = components
        .pop()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(KeyManagerError::UnsafePath)?
        .to_owned();
    let anchor = if path.is_absolute() { "/" } else { "." };
    let mut parent =
        Dir::open_ambient_dir(anchor, ambient_authority()).map_err(KeyManagerError::Io)?;
    for component in components {
        parent = parent
            .open_dir_nofollow(component)
            .map_err(KeyManagerError::Io)?;
    }
    Ok(ArtifactPath { parent, name })
}

fn validate_artifact_output(options: &Options) -> Result<()> {
    match options.out_path.as_deref() {
        Some("-") if options.json => Err(KeyManagerError::InvalidExchangeArtifact),
        Some("-") => Ok(()),
        Some(path) => {
            validate_relative_path(path)?;
            resolve_artifact_path(Path::new(path)).map(|_| ())
        }
        None => Err(KeyManagerError::InvalidExchangeArtifact),
    }
}

fn require_unused_artifact_output(options: &Options) -> Result<()> {
    let Some(path) = options.out_path.as_deref() else {
        return Err(KeyManagerError::InvalidExchangeArtifact);
    };
    if path == "-" {
        return Ok(());
    }
    let artifact = resolve_artifact_path(Path::new(path))?;
    match artifact.parent.symlink_metadata(&artifact.name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KeyManagerError::Io(error)),
        Ok(_) => Err(KeyManagerError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "artifact output already exists without committed replay",
        ))),
    }
}

fn write_committed_artifact(
    options: &Options,
    artifact: &str,
    result: &lunarwing::darkirc_key_manager::PreparedResult,
) -> Result<()> {
    let delivery_failed = || KeyManagerError::ArtifactDeliveryFailed {
        offer_id: result.offer_id.clone(),
    };
    match options.out_path.as_deref() {
        Some("-") => {
            let stdout = io::stdout();
            let mut output = stdout.lock();
            output
                .write_all(artifact.as_bytes())
                .and_then(|()| output.flush())
                .map_err(|_| delivery_failed())?;
            eprintln!(
                "offer_id={} contact_id={} fingerprint={} expires_at={}",
                result.offer_id, result.contact_id, result.public_fingerprint, result.expires_at
            );
        }
        Some(path) => {
            validate_relative_path(path).map_err(|_| delivery_failed())?;
            if artifact.len() > MAX_PENDING_BYTES {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            write_new_artifact(Path::new(path), artifact.as_bytes())
                .map_err(|_| delivery_failed())?;
            if options.json {
                print_json(result)?;
            }
        }
        None => return Err(KeyManagerError::InvalidExchangeArtifact),
    }
    Ok(())
}

fn write_new_artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    write_new_artifact_with_sync(path, bytes, sync_artifact_directory)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactWriteCrashPoint {
    AfterFinalRename,
}

fn sync_artifact_directory(directory: &Dir) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    directory
        .open_with(".", &options)
        .map_err(KeyManagerError::Io)?
        .into_std()
        .sync_all()
        .map_err(KeyManagerError::Io)
}

fn write_new_artifact_with_sync(
    path: &Path,
    bytes: &[u8],
    sync_directory: impl FnOnce(&Dir) -> Result<()>,
) -> Result<()> {
    write_new_artifact_inner(path, bytes, None, sync_directory)
}

fn write_new_artifact_inner(
    path: &Path,
    bytes: &[u8],
    crash_point: Option<ArtifactWriteCrashPoint>,
    sync_directory: impl FnOnce(&Dir) -> Result<()>,
) -> Result<()> {
    let artifact = resolve_artifact_path(path)?;
    match artifact.parent.symlink_metadata(&artifact.name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(KeyManagerError::Io(error)),
        Ok(metadata) => {
            return validate_existing_artifact(&artifact, &metadata, bytes, sync_directory);
        }
    }
    let temporary = format!(".{}.{}.tmp", artifact.name, random_exchange_id());
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .follow(FollowSymlinks::No)
        .sync(true);
    let publish_result = (|| {
        let mut file = artifact
            .parent
            .open_with(&temporary, &options)
            .map_err(KeyManagerError::Io)?
            .into_std();
        file.write_all(bytes).map_err(KeyManagerError::Io)?;
        file.sync_all().map_err(KeyManagerError::Io)?;
        publish_artifact_no_replace(&artifact.parent, &temporary, &artifact.name)
    })();
    if let Err(error) = publish_result {
        let _ = artifact.parent.remove_file(&temporary);
        return Err(error);
    }
    if crash_point == Some(ArtifactWriteCrashPoint::AfterFinalRename) {
        return Err(KeyManagerError::CrashInjected);
    }
    sync_directory(&artifact.parent)
}

#[cfg(target_os = "linux")]
fn publish_artifact_no_replace(parent: &Dir, temporary: &str, final_name: &str) -> Result<()> {
    nix::fcntl::renameat2(
        parent.as_fd(),
        temporary,
        parent.as_fd(),
        final_name,
        nix::fcntl::RenameFlags::RENAME_NOREPLACE,
    )
    .map_err(|error| KeyManagerError::Io(io::Error::from_raw_os_error(error as i32)))
}

#[cfg(not(target_os = "linux"))]
fn publish_artifact_no_replace(parent: &Dir, temporary: &str, final_name: &str) -> Result<()> {
    parent
        .hard_link(temporary, parent, final_name)
        .map_err(KeyManagerError::Io)?;
    parent.remove_file(temporary).map_err(KeyManagerError::Io)
}

fn validate_existing_artifact(
    artifact: &ArtifactPath,
    metadata: &cap_std::fs::Metadata,
    expected: &[u8],
    sync_directory: impl FnOnce(&Dir) -> Result<()>,
) -> Result<()> {
    if !metadata.is_file() || cap_fs_ext::MetadataExt::nlink(metadata) != 1 {
        return Err(KeyManagerError::UnsafePath);
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = artifact
        .parent
        .open_with(&artifact.name, &options)
        .map_err(KeyManagerError::Io)?
        .into_std();
    let opened = file.metadata().map_err(KeyManagerError::Io)?;
    if !opened.is_file()
        || opened.nlink() != 1
        || opened.uid() != nix::unistd::geteuid().as_raw()
        || opened.permissions().mode() & 0o7777 != 0o600
        || opened.dev() != cap_std::fs::MetadataExt::dev(metadata)
        || opened.ino() != cap_std::fs::MetadataExt::ino(metadata)
    {
        return Err(KeyManagerError::UnsafePath);
    }
    let existing = read_bounded_artifact(&mut file)?;
    let current = artifact
        .parent
        .symlink_metadata(&artifact.name)
        .map_err(KeyManagerError::Io)?;
    if !current.is_file()
        || cap_fs_ext::MetadataExt::nlink(&current) != 1
        || cap_std::fs::MetadataExt::dev(&current) != opened.dev()
        || cap_std::fs::MetadataExt::ino(&current) != opened.ino()
    {
        return Err(KeyManagerError::UnsafePath);
    }
    if existing.as_slice() != expected {
        return Err(KeyManagerError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "artifact output already exists with different content",
        )));
    }
    sync_directory(&artifact.parent)?;
    Ok(())
}

fn select_status_contact(contacts: Vec<ContactSummary>, name: &str) -> Result<StatusOutput> {
    let selected = contacts
        .into_iter()
        .filter(|item| item.name == name)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(KeyManagerError::ContactNotFound);
    }
    Ok(StatusOutput { contacts: selected })
}

fn read_migration_input(reader: impl Read) -> Result<Zeroizing<Vec<u8>>> {
    let max_read = u64::try_from(MAX_MIGRATION_BYTES)
        .map_err(|_| KeyManagerError::InvalidLedger)?
        .saturating_add(1);
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_MIGRATION_BYTES + 1));
    reader
        .take(max_read)
        .read_to_end(&mut bytes)
        .map_err(KeyManagerError::Io)?;
    if bytes.len() > MAX_MIGRATION_BYTES {
        return Err(KeyManagerError::InvalidLedger);
    }
    Ok(bytes)
}

fn read_baseline_input(reader: impl Read) -> Result<Zeroizing<Vec<u8>>> {
    let max_read = u64::try_from(MAX_BASELINE_BYTES)
        .map_err(|_| KeyManagerError::InvalidLedger)?
        .saturating_add(1);
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_BASELINE_BYTES + 1));
    reader
        .take(max_read)
        .read_to_end(&mut bytes)
        .map_err(KeyManagerError::Io)?;
    if bytes.len() > MAX_BASELINE_BYTES {
        return Err(KeyManagerError::InvalidLedger);
    }
    Ok(bytes)
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let mut output = serde_json::to_vec(value).map_err(|_| KeyManagerError::InvalidLedger)?;
    output.push(b'\n');
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writer
        .write_all(&output)
        .and_then(|()| writer.flush())
        .map_err(KeyManagerError::Io)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io::{self, Cursor};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn migration_status_output_contains_only_static_status() {
        let output = serde_json::to_string(&MigrationStatus {
            status: "staged",
            scope_id: None,
        })
        .expect("serialize migration status");

        assert_eq!(output, r#"{"status":"staged"}"#);
        assert!(!output.contains("contacts_toml"));
        assert!(!output.contains("my_dm_chacha_secret"));
    }

    #[test]
    fn migration_validation_status_contains_only_scope() {
        let output = serde_json::to_string(&MigrationStatus {
            status: "valid",
            scope_id: Some("00112233445566778899aabbccddeeff"),
        })
        .expect("serialize validation status");

        assert_eq!(
            output,
            r#"{"status":"valid","scope_id":"00112233445566778899aabbccddeeff"}"#
        );
        assert!(!output.contains("contacts_toml"));
        assert!(!output.contains("my_dm_chacha_secret"));
    }

    #[test]
    fn status_lookup_rejects_unknown_contact() {
        let error = select_status_contact(Vec::new(), "missing")
            .expect_err("unknown contact must return nonzero error");

        assert!(matches!(error, KeyManagerError::ContactNotFound));
    }

    #[test]
    fn migration_stdin_reader_rejects_oversized_payload() {
        let input = vec![b'x'; MAX_MIGRATION_BYTES + 1];

        let error = read_migration_input(Cursor::new(input)).expect_err("oversized stdin");

        assert!(matches!(error, KeyManagerError::InvalidLedger));
    }

    #[test]
    fn baseline_stdin_reader_rejects_oversized_payload() {
        let input = vec![b'x'; MAX_BASELINE_BYTES + 1];

        let error = read_baseline_input(Cursor::new(input)).expect_err("oversized baseline stdin");

        assert!(matches!(error, KeyManagerError::InvalidLedger));
    }

    #[test]
    fn artifact_stdin_reader_rejects_max_plus_one() {
        let error = read_bounded_artifact(Cursor::new(vec![b'x'; MAX_PENDING_BYTES + 1]))
            .expect_err("oversized artifact");
        assert!(matches!(error, KeyManagerError::InvalidExchangeArtifact));
    }

    #[test]
    fn ttl_and_stdout_mode_are_validated_before_exchange() {
        assert_eq!(
            validated_ttl(None).expect("default ttl"),
            DEFAULT_OFFER_TTL_SECONDS
        );
        for invalid in [i64::MIN, -1, 0, DEFAULT_OFFER_TTL_SECONDS + 1] {
            assert!(matches!(
                validated_ttl(Some(invalid)),
                Err(KeyManagerError::InvalidExchangeArtifact)
            ));
        }
        assert!(exchange_deadline(i64::MAX, 1).is_err());
        let options = Options {
            out_path: Some("-".to_owned()),
            json: true,
            ..Options::default()
        };
        assert!(matches!(
            validate_artifact_output(&options),
            Err(KeyManagerError::InvalidExchangeArtifact)
        ));
    }

    #[test]
    fn artifact_retry_accepts_only_safe_identical_existing_output() {
        assert!(matches!(
            validate_relative_path("lunarwing/state/darkirc/darkirc_config.toml"),
            Err(KeyManagerError::UnsafePath)
        ));
        let temp = tempdir().expect("artifact directory");
        let output = temp.path().join("offer.json");
        write_new_artifact(&output, b"first").expect("first artifact write");
        write_new_artifact(&output, b"first").expect("identical retry");
        let error = write_new_artifact(&output, b"second").expect_err("must not overwrite");
        assert!(matches!(error, KeyManagerError::Io(_)));
        assert_eq!(fs::read(&output).expect("read artifact"), b"first");

        let hardlink = temp
            .path()
            .join(".offer.json.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tmp");
        fs::hard_link(&output, &hardlink).expect("hard link artifact");
        assert!(matches!(
            write_new_artifact(&output, b"first"),
            Err(KeyManagerError::UnsafePath)
        ));
        assert_eq!(fs::metadata(&output).expect("hardlinked output").nlink(), 2);
        assert_eq!(fs::read(&hardlink).expect("arbitrary hardlink"), b"first");
        fs::remove_file(&hardlink).expect("remove hard link");

        fs::set_permissions(&output, fs::Permissions::from_mode(0o644))
            .expect("make artifact unsafe");
        assert!(matches!(
            write_new_artifact(&output, b"first"),
            Err(KeyManagerError::UnsafePath)
        ));

        let real_parent = temp.path().join("real-parent");
        fs::create_dir(&real_parent).expect("real artifact parent");
        let linked_parent = temp.path().join("linked-parent");
        std::os::unix::fs::symlink(&real_parent, &linked_parent).expect("artifact parent symlink");
        assert!(matches!(
            write_new_artifact(&linked_parent.join("offer.json"), b"blocked"),
            Err(KeyManagerError::Io(_)) | Err(KeyManagerError::UnsafePath)
        ));

        let symlink_output = temp.path().join("symlink.json");
        std::os::unix::fs::symlink(&output, &symlink_output).expect("artifact symlink");
        assert!(matches!(
            write_new_artifact(&symlink_output, b"first"),
            Err(KeyManagerError::UnsafePath)
        ));
    }

    #[test]
    fn artifact_retry_recovers_after_final_rename_precedes_sync_failure() {
        let temp = tempdir().expect("artifact directory");
        let output = temp.path().join("offer.json");
        let error = write_new_artifact_with_sync(&output, b"committed", |_| {
            Err(KeyManagerError::Io(io::Error::other(
                "injected directory sync failure",
            )))
        })
        .expect_err("sync failure reported after final rename");
        assert!(matches!(error, KeyManagerError::Io(_)));
        assert_eq!(fs::read(&output).expect("linked artifact"), b"committed");

        let sync_calls = Cell::new(0);
        write_new_artifact_with_sync(&output, b"committed", |directory| {
            sync_artifact_directory(directory)?;
            sync_calls.set(sync_calls.get() + 1);
            Ok(())
        })
        .expect("retry accepts and syncs committed artifact");
        assert_eq!(sync_calls.get(), 1);
    }

    #[test]
    fn artifact_retry_recovers_after_final_rename_precedes_directory_sync() {
        let temp = tempdir().expect("artifact directory");
        let output = temp.path().join("offer.json");
        let error = write_new_artifact_inner(
            &output,
            b"committed",
            Some(ArtifactWriteCrashPoint::AfterFinalRename),
            sync_artifact_directory,
        )
        .expect_err("crash after final rename");
        assert!(matches!(error, KeyManagerError::CrashInjected));
        assert_eq!(fs::metadata(&output).expect("renamed artifact").nlink(), 1);
        assert_eq!(
            fs::read_dir(temp.path())
                .expect("artifact directory")
                .count(),
            1
        );

        let sync_calls = Cell::new(0);
        write_new_artifact_with_sync(&output, b"committed", |directory| {
            sync_artifact_directory(directory)?;
            sync_calls.set(sync_calls.get() + 1);
            Ok(())
        })
        .expect("retry syncs renamed artifact");

        assert_eq!(fs::read(&output).expect("recovered artifact"), b"committed");
        assert_eq!(
            fs::metadata(&output).expect("recovered metadata").nlink(),
            1
        );
        assert_eq!(sync_calls.get(), 1);
        assert_eq!(
            fs::read_dir(temp.path())
                .expect("artifact directory")
                .count(),
            1
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_artifact_fallback_publishes_single_link_without_overwrite() {
        let temp = tempdir().expect("artifact directory");
        let output = temp.path().join("offer.json");

        write_new_artifact(&output, b"first").expect("fallback publication");
        assert_eq!(fs::metadata(&output).expect("artifact metadata").nlink(), 1);
        assert!(write_new_artifact(&output, b"different").is_err());
        assert_eq!(fs::read(&output).expect("artifact bytes"), b"first");
    }

    #[test]
    fn committed_artifact_failure_reports_offer_id() {
        let temp = tempdir().expect("artifact directory");
        let output = temp.path().join("offer.json");
        fs::write(&output, b"occupied").expect("occupied output");
        let options = Options {
            out_path: Some(output.to_string_lossy().into_owned()),
            ..Options::default()
        };
        let result = lunarwing::darkirc_key_manager::PreparedResult {
            offer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            contact_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            role: "initiator_offer".to_owned(),
            public_artifact: "{}".to_owned(),
            public_fingerprint: format!("sha256:{}", "a".repeat(64)),
            expires_at: 1,
        };

        assert!(matches!(
            write_committed_artifact(&options, &result.public_artifact, &result),
            Err(KeyManagerError::ArtifactDeliveryFailed { offer_id })
                if offer_id == result.offer_id
        ));
    }

    fn generator_pair() -> (String, String) {
        let secret = [7_u8; 32];
        let public = X25519PublicKey::from(&StaticSecret::from(secret));
        (
            bs58::encode(public.as_bytes()).into_string(),
            bs58::encode(secret).into_string(),
        )
    }

    #[test]
    fn generator_runner_captures_bounded_success_output() {
        let (public, secret) = generator_pair();
        let script = format!("printf 'Public: {public}\\nSecret: {secret}\\n'");
        let mut command = Command::new("sh");
        command.arg("-c").arg(script).env_clear();

        let (mut stdout, mut stderr, success) =
            capture_generator_output(command, Duration::from_secs(1), 4096).expect("run generator");
        let output = parse_generator_output(&mut stdout, &mut stderr, success)
            .expect("parse captured generator output");

        assert_eq!(output.public_key, public);
    }

    #[test]
    fn parse_generator_output_accepts_darkirc_toml_snippet() {
        // The pinned darkirc daemon prints a config block, not Public:/Secret:
        // lines; the helper must extract the real keys and ignore the
        // YOUR_CONTACT_PUBLIC_KEY placeholder.
        let (public, secret) = generator_pair();
        let mut stdout = format!(
            "Place this in your config file under your contact, you can reuse this keypair for multiple contacts\n\n[contact.\"satoshi\"]\ndm_chacha_public = \"YOUR_CONTACT_PUBLIC_KEY\"\nmy_dm_chacha_secret = \"{secret}\"\n#my_dm_chacha_public = \"{public}\"\n"
        )
        .into_bytes();
        let mut stderr = Vec::new();

        let output = parse_generator_output(&mut stdout, &mut stderr, true)
            .expect("darkirc TOML snippet parses");

        assert_eq!(output.public_key, public);
    }

    #[test]
    fn parse_generator_output_rejects_toml_snippet_without_public() {
        let (_public, secret) = generator_pair();
        let mut stdout =
            format!("[contact.\"satoshi\"]\nmy_dm_chacha_secret = \"{secret}\"\n").into_bytes();
        let mut stderr = Vec::new();

        assert!(parse_generator_output(&mut stdout, &mut stderr, true).is_err());
    }

    #[test]
    fn generator_runner_rejects_stdout_and_stderr_over_caps() {
        for script in ["printf '12345'", "printf '12345' >&2"] {
            let mut command = Command::new("sh");
            command.arg("-c").arg(script).env_clear();

            assert!(matches!(
                capture_generator_output(command, Duration::from_secs(1), 4),
                Err(KeyManagerError::InvalidCompatibility)
            ));
        }
    }

    #[test]
    fn generator_runner_enforces_combined_output_cap() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("printf '123'; printf '456' >&2")
            .env_clear();

        assert!(matches!(
            capture_generator_output(command, Duration::from_secs(1), 5),
            Err(KeyManagerError::InvalidCompatibility)
        ));
    }

    #[test]
    fn generator_runner_times_out_kills_and_reaps_child() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 5").env_clear();
        let started = Instant::now();

        let result = capture_generator_output(command, Duration::from_millis(50), 4096);

        assert!(matches!(result, Err(KeyManagerError::InvalidCompatibility)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn generator_parser_rejects_duplicate_recognized_fields_and_zeroizes_buffers() {
        let (public, secret) = generator_pair();
        let mut stdout =
            format!("Public: {public}\nPublic: {public}\nSecret: {secret}\n").into_bytes();
        let mut stderr = Vec::new();
        let error = match parse_generator_output(&mut stdout, &mut stderr, true) {
            Err(error) => error,
            Ok(_) => panic!("duplicate public field accepted"),
        };
        assert!(matches!(error, KeyManagerError::InvalidCompatibility));
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn generator_parser_rejects_unknown_output_and_wrong_pair() {
        let (public, secret) = generator_pair();
        let mut unknown = format!("Public: {public}\nSecret: {secret}\nnoise\n").into_bytes();
        assert!(parse_generator_output(&mut unknown, &mut Vec::new(), true).is_err());

        let mut wrong = format!(
            "Public: {}\nSecret: {secret}\n",
            bs58::encode([9_u8; 32]).into_string()
        )
        .into_bytes();
        assert!(parse_generator_output(&mut wrong, &mut Vec::new(), true).is_err());
    }

    struct FaultingReader {
        emitted: bool,
    }

    impl io::Read for FaultingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.emitted {
                return Err(io::Error::other("test read failure"));
            }
            let bytes = b"partial secret";
            let count = bytes.len().min(buffer.len());
            buffer[..count].copy_from_slice(&bytes[..count]);
            self.emitted = true;
            Ok(count)
        }
    }

    #[test]
    fn bounded_read_zeroizes_partial_input_on_error_path() {
        let error = read_baseline_input(FaultingReader { emitted: false })
            .expect_err("faulting reader must propagate the I/O error");

        assert!(matches!(error, KeyManagerError::Io(_)));
    }

    #[cfg(unix)]
    #[test]
    fn compatibility_attestation_rejects_non_attested_binary_path() {
        let temp = tempdir().expect("temporary binary directory");
        let binary_path = temp.path().join("darkirc");
        let contents = b"darkirc-test-binary";
        fs::write(&binary_path, contents).expect("write test binary");
        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))
            .expect("make test binary executable");
        let digest = format!("sha256:{:x}", Sha256::digest(contents));
        let options = Options {
            generator_profile: Some(
                lunarwing::darkirc_key_manager::MIGRATION_GENERATOR_PROFILE.to_owned(),
            ),
            binary_sha256: Some(digest),
            binary_path: Some(binary_path.to_string_lossy().into_owned()),
            key_format: Some(lunarwing::darkirc_key_manager::MIGRATION_KEY_FORMAT.to_owned()),
            source_revision: Some(
                lunarwing::darkirc_key_manager::MIGRATION_SOURCE_REVISION.to_owned(),
            ),
            ..Options::default()
        };

        let error = compatibility_attestation(&options)
            .expect_err("non-attested binary path must fail compatibility validation");

        assert!(matches!(error, KeyManagerError::InvalidCompatibility));
    }

    #[test]
    fn compatibility_attestation_rejects_special_permission_bits() {
        assert!(!attested_binary_mode(0o4755));
        assert!(!attested_binary_mode(0o2755));
        assert!(!attested_binary_mode(0o1755));
        assert!(attested_binary_mode(0o755));
    }

    #[test]
    fn release_id_and_helper_digest_bindings_fail_closed() {
        let valid = format!(
            "{}-{}-{}-{}",
            "a".repeat(40),
            "b".repeat(16),
            "c".repeat(16),
            "d".repeat(16)
        );
        assert!(valid_release_id(&valid));
        for invalid in ["../release", "A", "abc/def", "a-b-c"] {
            assert!(!valid_release_id(invalid));
        }

        let temp = tempdir().expect("helper digest fixture");
        let path = temp.path().join("helper");
        fs::write(&path, b"helper-generation-one").expect("write helper fixture");
        let mut file = fs::File::open(&path).expect("open helper fixture");
        let wrong = format!("sha256:{}", "0".repeat(64));
        assert!(matches!(
            verify_sha256(&mut file, &wrong),
            Err(KeyManagerError::InvalidCompatibility)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_daemon_fd_executes_measured_inode_after_path_replacement() {
        let temp = tempdir().expect("pinned daemon fixture");
        let daemon = temp.path().join("darkirc");
        let replacement = temp.path().join("replacement");
        fs::copy("/bin/true", &daemon).expect("copy measured executable");
        fs::copy("/bin/false", &replacement).expect("copy replacement executable");
        let opened = fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&daemon)
            .expect("open measured executable");
        fs::rename(&replacement, &daemon).expect("replace executable path");

        let status = Command::new(format!("/proc/self/fd/{}", opened.as_raw_fd()))
            .status()
            .expect("execute pinned descriptor");

        assert!(status.success());
    }
}
