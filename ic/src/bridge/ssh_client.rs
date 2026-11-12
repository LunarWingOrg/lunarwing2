//! In-process SSH client (Option 2) — runs a command on a remote host using the
//! russh client.
//!
//! This is the first live consumer of the `russh` client half (the harness
//! otherwise only uses russh's agent server, `ssh_agent.rs`). It is also
//! the first live wiring of [`HostKeyVerifier`](crate::bridge::ssh_hostkeys::HostKeyVerifier):
//! host-key verification runs inside russh's [`Handler::check_server_key`] during
//! key exchange, *before* authentication.
//!
//! Credentials are decoded from the encrypted secrets store (via
//! [`crate::bridge::ssh_agent::parse_key`]); keys never leave the daemon process.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Handle, Handler};
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::keys::{Algorithm, EcdsaCurve, PublicKeyBase64, PublicKeyOrCertificate};
use russh::{AlgorithmKind, ChannelMsg, Disconnect};

use crate::bridge::ssh::{SSHCredentials, SSHHostConfig, SshBridgeError};
use crate::bridge::ssh_agent::parse_key;
use crate::bridge::ssh_hostkeys::{HostKeyVerifier, is_rsa_host_key_algorithm};

/// Maximum bytes captured per stream (stdout / stderr). Output beyond this is
/// dropped and the result is flagged `truncated` to avoid unbounded buffering.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

const HOST_KEY_ALGORITHMS: &[Algorithm] = &[
    Algorithm::Ed25519,
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP256,
    },
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP384,
    },
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP521,
    },
];

fn client_config() -> client::Config {
    let mut config = client::Config::default();
    config.preferred.key = Cow::Borrowed(HOST_KEY_ALGORITHMS);
    config
}

fn map_connect_error(error: russh::Error) -> SshBridgeError {
    if let russh::Error::NoCommonAlgo {
        kind: AlgorithmKind::Key,
        theirs,
        ..
    } = &error
        && !theirs.is_empty()
        && theirs
            .iter()
            .all(|algorithm| is_rsa_host_key_algorithm(algorithm))
    {
        return SshBridgeError::ConnectionRefused(
            "the server offers only an RSA host key, which this build does not support (use Ed25519 or ECDSA host keys)"
                .to_string(),
        );
    }
    SshBridgeError::ConnectionRefused(error.to_string())
}

/// Key type and raw SSH wire blob of the host key the server presented.
///
/// `public_key_bytes()` yields the same representation `HostKeyVerifier`
/// compares against (it byte-compares, not fingerprint strings, so no
/// base64-padding mismatch). The client never offers a host key certificate
/// algorithm (`Preferred::host_key_certificates` stays empty) and pins are
/// plain host keys, so a certificate is refused instead of being trusted
/// without a configured authority.
fn presented_host_key(
    presented: &PublicKeyOrCertificate,
) -> Result<(String, Vec<u8>), SshBridgeError> {
    match presented {
        PublicKeyOrCertificate::PublicKey { key, .. } => {
            Ok((key.algorithm().to_string(), key.public_key_bytes()))
        }
        PublicKeyOrCertificate::Certificate(_) => Err(SshBridgeError::ConnectionRefused(
            "the server presented a host key certificate, which this build does not support (use a plain Ed25519 or ECDSA host key)"
                .to_string(),
        )),
    }
}

/// Result of a remote command execution.
#[derive(Debug)]
pub struct CommandResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    /// True if stdout or stderr hit [`MAX_OUTPUT_BYTES`] and was truncated.
    pub truncated: bool,
}

/// russh client handler. The only behavior we customize is host-key
/// verification, which delegates to the harness `HostKeyVerifier`.
struct ClientHandler {
    verifier: Arc<HostKeyVerifier>,
    host: SSHHostConfig,
    /// Set when `check_server_key` rejects a key, so callers can surface the
    /// precise reason instead of a generic connection error.
    reject_reason: Arc<std::sync::Mutex<Option<SshBridgeError>>>,
}

impl Handler for ClientHandler {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        let presented = presented_host_key(server_public_key);
        let verifier = Arc::clone(&self.verifier);
        let host = self.host.clone();
        let reject_reason = Arc::clone(&self.reject_reason);

        async move {
            let (key_type, key_bytes) = match presented {
                Ok(presented) => presented,
                Err(e) => {
                    if let Ok(mut slot) = reject_reason.lock() {
                        *slot = Some(e);
                    }
                    return Ok(false);
                }
            };
            match verifier
                .verify_from_config_with_key_type(&host, &key_type, &key_bytes)
                .await
            {
                Ok(_) => Ok(true),
                Err(e) => {
                    if let Ok(mut slot) = reject_reason.lock() {
                        *slot = Some(e);
                    }
                    // Returning Ok(false) aborts the handshake; the precise
                    // reason was stashed above for the caller to report.
                    Ok(false)
                }
            }
        }
    }
}

async fn connect_verified(
    host: &SSHHostConfig,
    verifier: Arc<HostKeyVerifier>,
) -> Result<Handle<ClientHandler>, SshBridgeError> {
    let reject_reason = Arc::new(std::sync::Mutex::new(None));
    let handler = ClientHandler {
        verifier,
        host: host.clone(),
        reject_reason: Arc::clone(&reject_reason),
    };
    let connect_secs = host.connect_timeout_secs.max(1);
    let connect_res = tokio::time::timeout(
        Duration::from_secs(connect_secs),
        client::connect(
            Arc::new(client_config()),
            (host.host.as_str(), host.port),
            handler,
        ),
    )
    .await;

    match connect_res {
        Err(_) => Err(SshBridgeError::ConnectionTimeout(connect_secs)),
        Ok(Err(error)) => {
            if let Some(reason) = reject_reason.lock().ok().and_then(|mut slot| slot.take()) {
                Err(match reason {
                    mismatch @ SshBridgeError::HostKeyMismatch { .. } => {
                        SshBridgeError::ConnectionRefused(mismatch.to_string())
                    }
                    other => other,
                })
            } else {
                Err(map_connect_error(error))
            }
        }
        Ok(Ok(session)) => Ok(session),
    }
}

/// Complete SSH key exchange and host-key verification without authenticating.
///
/// The `ssh_git` adapter uses this before launching OpenSSH. AcceptFirst pins
/// therefore reach encrypted persistence before any Git operation runs, and
/// the Git connection can enforce that newly materialized key strictly.
pub async fn connect_and_verify_host_key(
    host: &SSHHostConfig,
    verifier: Arc<HostKeyVerifier>,
) -> Result<(), SshBridgeError> {
    let session = connect_verified(host, verifier).await?;
    let _ = session
        .disconnect(Disconnect::ByApplication, "host key verified", "")
        .await;
    Ok(())
}

/// Connect to `host`, verify the server key, authenticate with `creds`, run
/// `command`, and return its output. Bounded by the host's connect/operation
/// timeouts.
pub async fn connect_and_exec(
    host: &SSHHostConfig,
    creds: &SSHCredentials,
    verifier: Arc<HostKeyVerifier>,
    command: &str,
) -> Result<CommandResult, SshBridgeError> {
    let key = parse_key(creds)?;
    let mut session = connect_verified(host, verifier).await?;

    // Authenticate with the decoded private key (Ed25519/ECDSA).
    let authenticated = session
        .authenticate_publickey(
            host.user.as_str(),
            PrivateKeyWithHashAlg::new(Arc::new(key), None),
        )
        .await
        .map_err(|e| SshBridgeError::Internal(format!("SSH authentication error: {e}")))?;
    if !authenticated.success() {
        return Err(SshBridgeError::AuthenticationFailed {
            user: host.user.clone(),
            host: host.host.clone(),
        });
    }

    // Run the command, bounded by the operation timeout.
    let op_secs = host.operation_timeout_secs.max(1);
    let result = match tokio::time::timeout(
        Duration::from_secs(op_secs),
        run_command(&mut session, command),
    )
    .await
    {
        Err(_) => {
            let _ = session.disconnect(Disconnect::ByApplication, "", "").await;
            return Err(SshBridgeError::ConnectionTimeout(op_secs));
        }
        Ok(r) => r?,
    };

    let _ = session.disconnect(Disconnect::ByApplication, "", "").await;
    Ok(result)
}

async fn run_command(
    session: &mut Handle<ClientHandler>,
    command: &str,
) -> Result<CommandResult, SshBridgeError> {
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| SshBridgeError::Internal(format!("failed to open SSH channel: {e}")))?;
    channel
        .exec(true, command.as_bytes())
        .await
        .map_err(|e| SshBridgeError::Internal(format!("failed to exec remote command: {e}")))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: Option<i32> = None;
    let mut truncated = false;

    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => append_capped(&mut stdout, &data, &mut truncated),
            // ext == 1 is SSH_EXTENDED_DATA_STDERR.
            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                append_capped(&mut stderr, &data, &mut truncated)
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = Some(exit_status as i32),
            Some(ChannelMsg::Eof) => {}
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }

    Ok(CommandResult {
        stdout,
        stderr,
        exit_code: exit_code.unwrap_or(-1),
        truncated,
    })
}

/// Append `data` to `buf`, capping total length at [`MAX_OUTPUT_BYTES`] and
/// setting `truncated` if the cap is hit.
fn append_capped(buf: &mut Vec<u8>, data: &[u8], truncated: &mut bool) {
    if buf.len() >= MAX_OUTPUT_BYTES {
        *truncated = true;
        return;
    }
    let remaining = MAX_OUTPUT_BYTES - buf.len();
    if data.len() > remaining {
        buf.extend_from_slice(&data[..remaining]);
        *truncated = true;
    } else {
        buf.extend_from_slice(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_common_key_error(theirs: &[&str]) -> russh::Error {
        russh::Error::NoCommonAlgo {
            kind: AlgorithmKind::Key,
            ours: vec!["ssh-ed25519".to_string()],
            theirs: theirs
                .iter()
                .map(|algorithm| (*algorithm).to_string())
                .collect(),
        }
    }

    #[test]
    fn client_config_offers_only_default_non_rsa_host_key_algorithms() {
        let expected = russh::Preferred::DEFAULT
            .key
            .iter()
            .filter(|algorithm| !matches!(algorithm, Algorithm::Rsa { .. }))
            .cloned()
            .collect::<Vec<_>>();
        assert!(expected.contains(&Algorithm::Ed25519));

        let config = client_config();
        assert_eq!(config.preferred.key.as_ref(), expected.as_slice());
        assert!(
            config
                .preferred
                .key
                .iter()
                .all(|algorithm| !matches!(algorithm, Algorithm::Rsa { .. }))
        );
    }

    #[test]
    fn rsa_only_negotiation_failure_names_host_key_type() {
        let error = no_common_key_error(&["rsa-sha2-512", "rsa-sha2-256", "ssh-rsa"]);
        let mapped = map_connect_error(error);
        assert!(
            matches!(&mapped, SshBridgeError::ConnectionRefused(message) if message.contains("only an RSA host key") && message.contains("Ed25519 or ECDSA")),
            "got {mapped:?}"
        );
    }

    #[test]
    fn mixed_host_key_negotiation_failure_keeps_original_error() {
        let error = no_common_key_error(&["ecdsa-sha2-nistp256", "ssh-rsa"]);
        let mapped = map_connect_error(error);
        assert!(
            matches!(&mapped, SshBridgeError::ConnectionRefused(message) if message.contains("No common Key algorithm") && !message.contains("only an RSA host key")),
            "got {mapped:?}"
        );
    }

    // Public test material only: a throwaway Ed25519 host key and a host
    // certificate for it. The private keys of both the host and the signing
    // authority were discarded when the fixture was generated.
    const HOST_ED25519_OPENSSH: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIF3FmAjkaEyp/3WAchHG1OYyRH1hSdWDe94cqAeHBroX lunarwing-test-host";
    const HOST_CERTIFICATE_OPENSSH: &str = "ssh-ed25519-cert-v01@openssh.com AAAAIHNzaC1lZDI1NTE5LWNlcnQtdjAxQG9wZW5zc2guY29tAAAAIHjQiLHhOemXSPUbuEnOPQUheNrF1kIZUfooBul8F1IgAAAAIF3FmAjkaEyp/3WAchHG1OYyRH1hSdWDe94cqAeHBroXAAAAAAAAAAAAAAACAAAAE2x1bmFyd2luZy10ZXN0LWhvc3QAAAAYAAAAFHRlc3QuZXhhbXBsZS5pbnZhbGlkAAAAAGlVuQAAAAAAfCRfAAAAAAAAAAAAAAAAAAAAADMAAAALc3NoLWVkMjU1MTkAAAAgLAj7dc1CEW2O1O69e25jc7KhcaShawvTmfdey9KTrQoAAABTAAAAC3NzaC1lZDI1NTE5AAAAQE4V2VgJhnPCnDIitXbHfRUEcaQuUgqDxTpqv3vzwspNrgKIsEYEWYztDCNWWQoiXq4dPPdiRKryBI5pT6nv2gw= lunarwing-test-host";

    #[test]
    fn plain_host_key_yields_its_type_and_wire_blob() {
        let key = russh::keys::PublicKey::from_openssh(HOST_ED25519_OPENSSH).unwrap();
        let (key_type, key_bytes) = presented_host_key(&PublicKeyOrCertificate::PublicKey {
            key: key.clone(),
            hash_alg: None,
        })
        .unwrap();
        assert_eq!(key_type, "ssh-ed25519");
        assert_eq!(key_bytes, key.public_key_bytes());
    }

    #[test]
    fn host_key_certificate_is_refused() {
        let certificate = russh::keys::Certificate::from_openssh(HOST_CERTIFICATE_OPENSSH).unwrap();
        let refused = presented_host_key(&PublicKeyOrCertificate::Certificate(certificate));
        assert!(
            matches!(&refused, Err(SshBridgeError::ConnectionRefused(message)) if message.contains("host key certificate")),
            "got {refused:?}"
        );
    }

    #[test]
    fn client_config_offers_no_host_key_certificate_algorithm() {
        assert!(client_config().preferred.host_key_certificates.is_empty());
    }

    #[test]
    fn test_append_capped_under_limit() {
        let mut buf = Vec::new();
        let mut truncated = false;
        append_capped(&mut buf, b"hello", &mut truncated);
        assert_eq!(buf, b"hello");
        assert!(!truncated);
    }

    #[test]
    fn test_append_capped_crosses_limit() {
        let mut buf = vec![0u8; MAX_OUTPUT_BYTES - 3];
        let mut truncated = false;
        append_capped(&mut buf, b"abcdef", &mut truncated);
        assert_eq!(buf.len(), MAX_OUTPUT_BYTES);
        assert!(truncated);
    }

    #[test]
    fn test_append_capped_already_full() {
        let mut buf = vec![0u8; MAX_OUTPUT_BYTES];
        let mut truncated = false;
        append_capped(&mut buf, b"x", &mut truncated);
        assert_eq!(buf.len(), MAX_OUTPUT_BYTES);
        assert!(truncated);
    }

    #[tokio::test]
    async fn test_mislabeled_rsa_key_rejected_before_connect() {
        let host = SSHHostConfig {
            host: "127.0.0.1".to_string(),
            port: 9,
            user: "nobody".to_string(),
            key_type: crate::bridge::ssh::SSHKeyType::Ed25519,
            host_key_mode: crate::bridge::ssh::HostKeyMode::Strict,
            known_host_key: None,
            connect_timeout_secs: 1,
            operation_timeout_secs: 1,
            keepalive_interval_secs: 0,
            keepalive_max_misses: 0,
        };
        let creds = crate::bridge::ssh_agent::tests::rsa_test_credentials();
        let error = connect_and_exec(&host, &creds, Arc::new(HostKeyVerifier::new()), "true")
            .await
            .unwrap_err();
        assert!(
            matches!(&error, SshBridgeError::InvalidKeyFormat(message) if message.contains("RSA")),
            "got {error:?}"
        );
    }
}
