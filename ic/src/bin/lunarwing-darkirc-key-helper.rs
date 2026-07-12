//! Short-lived, secret-minimizing DarkIRC config/adoption helper.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::ExitCode;

use lunarwing::darkirc_key_manager::{
    CompatibilityAttestation, ContactSummary, KeyManagerError, MAX_BASELINE_BYTES,
    MAX_MIGRATION_BYTES, Result, TenantPaths, adopt_contacts, doctor_contacts, export_migration,
    import_migration, inspect_contacts, migration_ready, read_ledger, recover, stage_migration,
    update_baseline, validate_migration, validate_migration_unbound,
};
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
