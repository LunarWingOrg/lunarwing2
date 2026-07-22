//! Short-lived, secret-minimizing DarkIRC config/adoption helper.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::process::{Command, ExitCode, Stdio};

use lunarwing::darkirc_key_manager::{
    CompatibilityAttestation, CompleteExchange, ContactSummary, DEFAULT_OFFER_TTL_SECONDS,
    ExchangeStatus, KeyManagerError, MAX_BASELINE_BYTES, MAX_MIGRATION_BYTES, MAX_PENDING_BYTES,
    PrepareExchange, PublicFingerprint, RespondExchange, Result, TenantPaths, adopt_contacts,
    cancel_exchange, complete_exchange, doctor_contacts, export_migration, import_migration,
    inspect_contacts, inspect_exchanges, migration_ready, prepare_exchange, read_ledger, recover,
    respond_exchange, stage_migration, update_baseline, validate_migration,
    validate_migration_unbound,
};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const ATTESTED_DARKIRC_PATH: &str = "/usr/local/bin/darkirc";

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
            "--expires" => {
                options.expires_seconds = Some(parse_i64(&next_value(&mut args)?)?)
            }
            "--defer-apply" => options.defer_apply = true,
            "--offer-id" => options.offer_id = Some(next_value(&mut args)?),
            "--contact-id" => options.contact_id = Some(next_value(&mut args)?),
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
            let scope = options.scope_id.take().ok_or(KeyManagerError::InvalidScope)?;
            let contact = options.contact.take().ok_or(KeyManagerError::InvalidContact)?;
            let attested = attestation
                .as_ref()
                .ok_or(KeyManagerError::InvalidCompatibility)?;
            let keypair = generate_keypair(options.binary_path.as_deref())?;
            let now = unix_now();
            let ttl = options.expires_seconds.unwrap_or(DEFAULT_OFFER_TTL_SECONDS);
            let offer_id = options.offer_id.take().unwrap_or_else(random_exchange_id);
            let contact_id =
                options.contact_id.take().unwrap_or_else(random_exchange_id);
            let request = PrepareExchange {
                contact_name: &contact,
                offer_id: &offer_id,
                contact_id: &contact_id,
                local_public: &keypair.public_key,
                local_private: &keypair.secret_key,
                intended_peer_contact_id: None,
                intended_peer_fingerprint: None,
                label: None,
                previous_fingerprint: None,
                generation: 0,
                created_at: now,
                expires_at: now + ttl,
            };
            let _ = attested;
            let result = prepare_exchange(&paths, &scope, &request)?;
            write_artifact_output(&options, &result.public_artifact, &result)
        }
        "respond" => {
            let scope = options.scope_id.take().ok_or(KeyManagerError::InvalidScope)?;
            let contact = options.contact.take().ok_or(KeyManagerError::InvalidContact)?;
            let expected_fp = options
                .expect_peer_fingerprint
                .as_deref()
                .ok_or(KeyManagerError::FingerprintMismatch)?;
            let expected_fp = PublicFingerprint::parse(expected_fp)?;
            let offer_bytes = read_artifact_input(&options)?;
            let keypair = generate_keypair(options.binary_path.as_deref())?;
            let now = unix_now();
            let ttl = options.expires_seconds.unwrap_or(DEFAULT_OFFER_TTL_SECONDS);
            let offer_id = options.offer_id.take().unwrap_or_else(random_exchange_id);
            let contact_id =
                options.contact_id.take().unwrap_or_else(random_exchange_id);
            let request = RespondExchange {
                contact_name: &contact,
                offer_id: &offer_id,
                contact_id: &contact_id,
                local_public: &keypair.public_key,
                local_private: &keypair.secret_key,
                initiator_offer_id: "",
                expected_initiator_fingerprint: &expected_fp,
                label: None,
                created_at: now,
                expires_at: now + ttl,
            };
            let result = respond_exchange(&paths, &scope, &offer_bytes, &request)?;
            write_artifact_output(&options, &result.public_artifact, &result)
        }
        "complete" => {
            let scope = options.scope_id.take().ok_or(KeyManagerError::InvalidScope)?;
            let contact = options.contact.take().ok_or(KeyManagerError::InvalidContact)?;
            let exchange_id = options
                .offer_id
                .take()
                .or_else(|| options.contact_id.take())
                .ok_or(KeyManagerError::InvalidExchangeArtifact)?;
            let expected_fp = options
                .expect_peer_fingerprint
                .as_deref()
                .ok_or(KeyManagerError::FingerprintMismatch)?;
            let expected_fp = PublicFingerprint::parse(expected_fp)?;
            let peer_bytes = read_artifact_input(&options)?;
            let request = CompleteExchange {
                exchange_id: &exchange_id,
                expected_peer_fingerprint: &expected_fp,
                defer_apply: true,
            };
            let _ = contact;
            let status = complete_exchange(&paths, &scope, &peer_bytes, &request)?;
            print_json(&status)
        }
        "cancel" => {
            let scope = options.scope_id.ok_or(KeyManagerError::InvalidScope)?;
            let exchange_id = options
                .offer_id
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
    secret_key: String,
}

fn generate_keypair(binary_path: Option<&str>) -> Result<KeypairOutput> {
    let binary = binary_path.unwrap_or(ATTESTED_DARKIRC_PATH);
    let output = Command::new(binary)
        .arg("--gen-chacha-keypair")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(KeyManagerError::Io)?;
    if !output.status.success() {
        return Err(KeyManagerError::InvalidCompatibility);
    }
    let stdout = Zeroizing::new(String::from_utf8_lossy(&output.stdout).into_owned());
    let mut public_key = None;
    let mut secret_key = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Public:") {
            public_key = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("Secret:") {
            secret_key = Some(rest.trim().to_owned());
        }
    }
    match (public_key, secret_key) {
        (Some(p), Some(s)) => Ok(KeypairOutput {
            public_key: p,
            secret_key: s,
        }),
        _ => Err(KeyManagerError::InvalidCompatibility),
    }
}

fn read_artifact_input(options: &Options) -> Result<Vec<u8>> {
    match options.in_path.as_deref() {
        Some("-") | None => {
            let mut buf = Vec::new();
            io::stdin()
                .take(u64::from(MAX_PENDING_BYTES as u32))
                .read_to_end(&mut buf)
                .map_err(KeyManagerError::Io)?;
            Ok(buf)
        }
        Some(path) => {
            if path.starts_with('/') {
                return Err(KeyManagerError::UnsafePath);
            }
            fs::read(path).map_err(KeyManagerError::Io)
        }
    }
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
            if path.starts_with('/') {
                return Err(KeyManagerError::UnsafePath);
            }
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true).mode(0o600);
            let mut file = opts.open(path).map_err(KeyManagerError::Io)?;
            file.write_all(artifact.as_bytes()).map_err(KeyManagerError::Io)?;
        }
    }
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
