//! Short-lived, secret-minimizing DarkIRC config/adoption helper.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};
use std::process::{Command, ExitCode, Stdio};

use lunarwing::darkirc_key_manager::{
    CompatibilityAttestation, CompleteExchange, ContactSummary, DEFAULT_OFFER_TTL_SECONDS,
    ExchangeRole, ExchangeStatus, KeyManagerError, MAX_BASELINE_BYTES, MAX_MIGRATION_BYTES,
    MAX_PENDING_BYTES, PrepareExchange, PublicFingerprint, RespondExchange, Result, TenantPaths,
    adopt_contacts, cancel_exchange, complete_exchange, doctor_contacts, export_migration,
    import_migration, inspect_contacts, inspect_exchanges, migration_ready, prepare_exchange,
    read_ledger, recover, respond_exchange, stage_migration, update_baseline, validate_migration,
    validate_migration_unbound, validate_public_exchange_at,
};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

const ATTESTED_DARKIRC_PATH: &str = "/usr/local/bin/darkirc";

#[derive(Debug)]
struct Options {
    tenant: Option<String>,
    scope_id: Option<String>,
    expected_uid: Option<u32>,
    expected_gid: Option<u32>,
    contact: Option<String>,
    generator_profile: Option<String>,
    binary_sha256: Option<String>,
    binary_path: Option<String>,
    key_format: Option<String>,
    source_revision: Option<String>,
    unbound_validation: bool,
    yes: bool,
    out_path: Option<String>,
    in_path: Option<String>,
    expect_peer_fingerprint: Option<String>,
    expires_seconds: Option<i64>,
    defer_apply: bool,
    offer_id: Option<String>,
    contact_id: Option<String>,
    exchange_id: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tenant: None,
            scope_id: None,
            expected_uid: None,
            expected_gid: None,
            contact: None,
            generator_profile: None,
            binary_sha256: None,
            binary_path: None,
            key_format: None,
            source_revision: None,
            unbound_validation: false,
            yes: false,
            out_path: None,
            in_path: None,
            expect_peer_fingerprint: None,
            expires_seconds: None,
            defer_apply: true,
            offer_id: None,
            contact_id: None,
            exchange_id: None,
        }
    }
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

fn main() -> ExitCode {
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
            "--key-format" => options.key_format = Some(next_value(&mut args)?),
            "--source-revision" => options.source_revision = Some(next_value(&mut args)?),
            "--unbound" => options.unbound_validation = true,
            "--yes" => options.yes = true,
            "--json" => {}
            "--out" => options.out_path = Some(next_value(&mut args)?),
            "--in" => options.in_path = Some(next_value(&mut args)?),
            "--expect-peer-fingerprint" => {
                options.expect_peer_fingerprint = Some(next_value(&mut args)?)
            }
            "--expires" => options.expires_seconds = Some(parse_i64(&next_value(&mut args)?)?),
            "--defer-apply" => options.defer_apply = true,
            "--offer-id" => options.offer_id = Some(next_value(&mut args)?),
            "--contact-id" => options.contact_id = Some(next_value(&mut args)?),
            "--exchange-id" => options.exchange_id = Some(next_value(&mut args)?),
            _ => return Err(KeyManagerError::InvalidTenant),
        }
    }

    let attestation = if command == "validate-migration" && options.unbound_validation {
        None
    } else {
        Some(compatibility_attestation(&options)?)
    };

    if command == "validate-migration" {
        let manifest = read_migration_input(io::stdin())?;
        let validation = if options.unbound_validation {
            validate_migration_unbound(&manifest)?
        } else {
            validate_migration(
                &manifest,
                attestation
                    .as_ref()
                    .ok_or(KeyManagerError::InvalidCompatibility)?,
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
                attestation
                    .as_ref()
                    .ok_or(KeyManagerError::InvalidCompatibility)?,
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
                attestation
                    .as_ref()
                    .ok_or(KeyManagerError::InvalidCompatibility)?,
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
                attestation
                    .as_ref()
                    .ok_or(KeyManagerError::InvalidCompatibility)?,
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
            let attested = attestation
                .as_ref()
                .ok_or(KeyManagerError::InvalidCompatibility)?;
            let keypair = generate_keypair()?;
            let now = unix_now();
            let ttl = options.expires_seconds.unwrap_or(DEFAULT_OFFER_TTL_SECONDS);
            let offer_id = options.offer_id.take().unwrap_or_else(random_exchange_id);
            let contact_id = options.contact_id.take().unwrap_or_else(random_exchange_id);
            let request = PrepareExchange {
                contact_name: &contact,
                offer_id: &offer_id,
                contact_id: &contact_id,
                local_public: &keypair.public_key,
                local_private: keypair.secret_key.expose_secret(),
                intended_peer_contact_id: None,
                intended_peer_fingerprint: None,
                label: None,
                previous_fingerprint: None,
                generation: 1,
                created_at: now,
                expires_at: now + ttl,
                now,
                attestation: attested,
            };
            let _ = attested;
            let result = prepare_exchange(&paths, &scope, &request)?;
            write_artifact_output(&options, &result.public_artifact, &result)
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
            let offer_bytes = read_artifact_input(&options)?;
            let initiator = validate_public_exchange_at(
                &offer_bytes,
                ExchangeRole::InitiatorOffer,
                attestation
                    .as_ref()
                    .ok_or(KeyManagerError::InvalidCompatibility)?,
                unix_now(),
            )?;
            let keypair = generate_keypair()?;
            let now = unix_now();
            let ttl = options.expires_seconds.unwrap_or(DEFAULT_OFFER_TTL_SECONDS);
            let offer_id = options.offer_id.take().unwrap_or_else(random_exchange_id);
            let contact_id = options.contact_id.take().unwrap_or_else(random_exchange_id);
            let request = RespondExchange {
                contact_name: &contact,
                offer_id: &offer_id,
                contact_id: &contact_id,
                local_public: &keypair.public_key,
                local_private: keypair.secret_key.expose_secret(),
                initiator_offer_id: &initiator.offer_id,
                expected_initiator_fingerprint: &expected_fp,
                label: None,
                created_at: now,
                expires_at: now + ttl,
                now,
                attestation: attestation
                    .as_ref()
                    .ok_or(KeyManagerError::InvalidCompatibility)?,
            };
            let result = respond_exchange(&paths, &scope, &offer_bytes, &request)?;
            write_artifact_output(&options, &result.public_artifact, &result)
        }
        "complete" => {
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
                defer_apply: true,
                now: unix_now(),
                attestation: attestation
                    .as_ref()
                    .ok_or(KeyManagerError::InvalidCompatibility)?,
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

fn compatibility_attestation(options: &Options) -> Result<CompatibilityAttestation> {
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
    if binary_path != ATTESTED_DARKIRC_PATH {
        return Err(KeyManagerError::InvalidCompatibility);
    }
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
    let mut file = fs::File::open(binary_path).map_err(KeyManagerError::Io)?;
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
    Ok(attestation)
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

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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

fn generate_keypair() -> Result<KeypairOutput> {
    let mut output = Command::new(ATTESTED_DARKIRC_PATH)
        .arg("--gen-chacha-keypair")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(KeyManagerError::Io)?;
    parse_generator_output(
        &mut output.stdout,
        &mut output.stderr,
        output.status.success(),
    )
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
        let mut public_key = None;
        let mut secret_key = None;
        for line in text.lines() {
            let (slot, value) = if let Some(value) = line.strip_prefix("Public:") {
                (&mut public_key, value)
            } else if let Some(value) = line.strip_prefix("Secret:") {
                (&mut secret_key, value)
            } else {
                return Err(KeyManagerError::InvalidCompatibility);
            };
            let value = value.trim();
            if value.is_empty() || slot.replace(value.to_owned()).is_some() {
                return Err(KeyManagerError::InvalidCompatibility);
            }
        }
        let public_key = public_key.ok_or(KeyManagerError::InvalidCompatibility)?;
        let secret_key = secret_key.ok_or(KeyManagerError::InvalidCompatibility)?;
        validate_generated_pair(&public_key, &secret_key)?;
        Ok(KeypairOutput {
            public_key,
            secret_key: SecretString::from(secret_key),
        })
    })();
    stdout.zeroize();
    stderr.zeroize();
    parsed
}

fn decode_generator_key(value: &str) -> Result<Zeroizing<[u8; 32]>> {
    let decoded = Zeroizing::new(
        bs58::decode(value)
            .into_vec()
            .map_err(|_| KeyManagerError::InvalidCompatibility)?,
    );
    if decoded.len() != 32
        || decoded.iter().all(|byte| *byte == 0)
        || bs58::encode(decoded.as_slice()).into_string() != value
    {
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
            let file = fs::File::open(path).map_err(KeyManagerError::Io)?;
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
    Ok(())
}

fn write_artifact_output(
    options: &Options,
    artifact: &str,
    result: &lunarwing::darkirc_key_manager::PreparedResult,
) -> Result<()> {
    match options.out_path.as_deref() {
        Some("-") | None => {
            print!("{artifact}");
            eprintln!(
                "offer_id={} contact_id={} fingerprint={} expires_at={}",
                result.offer_id, result.contact_id, result.public_fingerprint, result.expires_at
            );
        }
        Some(path) => {
            validate_relative_path(path)?;
            if artifact.len() > MAX_PENDING_BYTES {
                return Err(KeyManagerError::InvalidExchangeArtifact);
            }
            write_new_artifact(Path::new(path), artifact.as_bytes())?;
        }
    }
    Ok(())
}

fn write_new_artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(KeyManagerError::UnsafePath)?;
    let temporary = parent.join(format!(".{name}.{}.tmp", random_exchange_id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let write_result = (|| {
        let mut file = options.open(&temporary).map_err(KeyManagerError::Io)?;
        file.write_all(bytes).map_err(KeyManagerError::Io)?;
        file.sync_all().map_err(KeyManagerError::Io)?;
        fs::hard_link(&temporary, path).map_err(KeyManagerError::Io)?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(KeyManagerError::Io)
    })();
    let _ = fs::remove_file(&temporary);
    write_result
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
    let output = serde_json::to_string(value).map_err(|_| KeyManagerError::InvalidLedger)?;
    println!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
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

    fn generator_pair() -> (String, String) {
        let secret = [7_u8; 32];
        let public = X25519PublicKey::from(&StaticSecret::from(secret));
        (
            bs58::encode(public.as_bytes()).into_string(),
            bs58::encode(secret).into_string(),
        )
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
}
