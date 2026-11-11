//! SSH Agent Server — Handles ssh-agent protocol for worker authentication.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::Future;
use russh::keys::PrivateKey;
use russh::keys::agent::server::{Agent, MessageType};
use secrecy::ExposeSecret;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_stream::wrappers::UnixListenerStream;
use tracing::{error, info, warn};

use crate::bridge::ssh::{Result, SSHCredentials, SshBridgeError};

#[derive(Clone)]
pub struct SshAgent {
    keys: Arc<Mutex<HashMap<String, Arc<PrivateKey>>>>,
}

impl Default for SshAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl SshAgent {
    pub fn new() -> Self {
        Self {
            keys: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn add_key(&self, hostname: String, creds: SSHCredentials) -> Result<()> {
        let key_pair = parse_key(&creds)?;
        let mut keys = self.keys.lock().await;
        keys.insert(hostname.clone(), Arc::new(key_pair));
        info!("Added key to SSH agent for host {}", hostname);
        Ok(())
    }

    pub async fn remove_key(&self, hostname: &str) -> Result<bool> {
        let mut keys = self.keys.lock().await;
        let removed = keys.remove(hostname).is_some();
        if removed {
            info!("Removed key from SSH agent for host {}", hostname);
        }
        Ok(removed)
    }

    pub async fn list_keys(&self) -> Vec<String> {
        let keys = self.keys.lock().await;
        keys.keys().cloned().collect()
    }
}

pub(crate) fn parse_key(creds: &SSHCredentials) -> Result<PrivateKey> {
    let key_str = String::from_utf8_lossy(&creds.key_data).to_string();
    let passphrase = creds.passphrase.as_ref().map(|s| s.expose_secret());

    // Use the internal format decoder - it's public in the crate root
    let key = russh::keys::decode_secret_key(&key_str, passphrase)
        .map_err(|e| SshBridgeError::InvalidKeyFormat(format!("Key parse error: {}", e)))?;
    if matches!(key.algorithm(), russh::keys::Algorithm::Rsa { .. }) {
        return Err(SshBridgeError::InvalidKeyFormat(
            "RSA keys are not supported (use Ed25519 or ECDSA)".to_string(),
        ));
    }
    Ok(key)
}

impl Agent for SshAgent {
    fn confirm(
        self,
        _pk: Arc<PrivateKey>,
    ) -> Box<dyn Future<Output = (Self, bool)> + Unpin + Send> {
        Box::new(futures::future::ready((self, true)))
    }

    async fn confirm_request(&self, _msg: MessageType) -> bool {
        true
    }
}

pub struct SshAgentServer {
    socket_path: PathBuf,
    keys: Arc<Mutex<HashMap<String, Arc<PrivateKey>>>>,
    _join_handle: tokio::task::JoinHandle<()>,
}

impl Drop for SshAgentServer {
    fn drop(&mut self) {
        self._join_handle.abort();
        // Best-effort key clearing: try_lock avoids panicking when Drop runs
        // inside a tokio runtime (blocking_lock would). If the lock is
        // contended, the keys will be zeroized when the last Arc clone drops.
        if let Ok(mut keys) = self.keys.try_lock() {
            keys.clear();
        }
        let _ = std::fs::remove_file(&self.socket_path);
        info!("SSH agent server stopped: {}", self.socket_path.display());
    }
}

impl SshAgentServer {
    pub async fn start(
        socket_path: PathBuf,
        keys: HashMap<String, SSHCredentials>,
    ) -> Result<Arc<Self>> {
        // Workers bind-mount the socket's PARENT DIRECTORY (not the socket
        // file), so a daemon restart that binds a fresh socket inode is picked
        // up without recreating the worker containers. That only works if the
        // directory exists, so create it here — the caller only guarantees the
        // run dir root. Explicit 0o755: the daemon unit runs with UMask=0077,
        // and a daemon-created directory would otherwise be 0700, blocking
        // worker UIDs (which differ from the daemon's) from traversing to the
        // 0o666 socket.
        #[cfg(unix)]
        let socket_dir_permissions = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::Permissions::from_mode(0o755)
        };
        #[cfg(not(unix))]
        let socket_dir_permissions = std::fs::Permissions::from(0o755);
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                warn!(
                    "Failed to create SSH agent socket directory {}: {}",
                    parent.display(),
                    e
                );
                SshBridgeError::AgentSocketUnavailable
            })?;
            let _ = std::fs::set_permissions(parent, socket_dir_permissions);
        }
        if socket_path.exists() {
            warn!(
                "SSH agent socket exists, removing: {}",
                socket_path.display()
            );
            let _ = std::fs::remove_file(&socket_path);
        }

        let listener = UnixListener::bind(&socket_path).map_err(|e| {
            warn!("Failed to bind SSH agent socket: {}", e);
            SshBridgeError::AgentSocketUnavailable
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 0o666: the socket is in the tenant's run dir (not /tmp), and
            // rootless podman maps the host UID to root inside the container.
            // Worker processes run as a different user than the daemon and
            // need read+write access to the socket. The run dir itself is
            // tenant-owned, so this doesn't expose the socket to other tenants.
            let perms = std::fs::Permissions::from_mode(0o666);
            let _ = std::fs::set_permissions(&socket_path, perms);
        }

        info!("SSH agent server listening: {}", socket_path.display());

        // Parse keys upfront so we can add them via the agent client protocol
        // after the server starts. The russh agent server maintains its
        // OWN internal KeyStore (separate from SshAgent.keys), so keys must be
        // added via the agent protocol (ADD_IDENTITY message) — not just
        // stored in the SshAgent struct.
        let mut parsed_keys: Vec<(String, PrivateKey)> = Vec::new();
        for (hostname, creds) in keys {
            match parse_key(&creds) {
                Ok(key_pair) => {
                    parsed_keys.push((hostname, key_pair));
                }
                Err(e) => warn!("Failed to parse key for {}: {}", hostname, e),
            }
        }

        let keys_map: Arc<Mutex<HashMap<String, Arc<PrivateKey>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let keys_clone = Arc::clone(&keys_map);
        let socket_path_for_log = socket_path.clone();
        let socket_path_for_client = socket_path.clone();

        let join_handle = tokio::spawn(async move {
            let stream = UnixListenerStream::new(listener);
            let agent = SshAgent { keys: keys_clone };
            if let Err(e) = russh::keys::agent::server::serve(stream, agent).await {
                error!(
                    "SSH agent server error on {}: {}",
                    socket_path_for_log.display(),
                    e
                );
            }
        });

        // Give the server a moment to start accepting connections, then add
        // keys via the agent client protocol so they land in the server's
        // internal KeyStore (which the server reads for REQUEST_IDENTITIES
        // and SIGN requests).
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        for (hostname, key_pair) in parsed_keys {
            // Store in our map first (for status reporting via the API) since
            // add_identity takes a reference and doesn't consume the key.
            let key_arc = Arc::new(key_pair.clone());
            {
                let mut guard = keys_map.lock().await;
                guard.insert(hostname.clone(), key_arc);
            }
            match russh::keys::agent::client::AgentClient::connect_uds(&socket_path_for_client)
                .await
            {
                Ok(mut client) => {
                    if let Err(e) = client.add_identity(&key_pair, &[]).await {
                        warn!(
                            "Failed to add key for {} via agent protocol: {}",
                            hostname, e
                        );
                    } else {
                        info!("Added key for {} to SSH agent via protocol", hostname);
                    }
                }
                Err(e) => {
                    warn!("Failed to connect to agent client for {}: {}", hostname, e);
                }
            }
        }

        Ok(Arc::new(Self {
            socket_path,
            keys: keys_map,
            _join_handle: join_handle,
        }))
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub async fn add_key(&self, hostname: String, creds: SSHCredentials) -> Result<()> {
        let key_pair = parse_key(&creds)?;
        let mut keys = self.keys.lock().await;
        keys.insert(hostname.clone(), Arc::new(key_pair));
        info!("Added key to SSH agent for host {}", hostname);
        Ok(())
    }

    pub async fn remove_key(&self, hostname: &str) -> Result<bool> {
        let mut keys = self.keys.lock().await;
        let removed = keys.remove(hostname).is_some();
        if removed {
            info!("Removed key from SSH agent for host {}", hostname);
        }
        Ok(removed)
    }

    pub async fn list_keys(&self) -> Vec<String> {
        let keys = self.keys.lock().await;
        keys.keys().cloned().collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroize::Zeroizing;

    const RSA_TEST_KEY: &str = concat!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n", // no-secret-scan: upstream test fixture, not a credential; gitleaks:allow
        "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\n",
        "-----END OPENSSH PRIVATE KEY-----"
    );

    pub(crate) fn rsa_test_credentials() -> SSHCredentials {
        SSHCredentials {
            key_data: Zeroizing::new(RSA_TEST_KEY.as_bytes().to_vec()),
            passphrase: None,
        }
    }

    #[tokio::test]
    async fn test_agent_server_start_stop() {
        let temp_dir = TempDir::new().unwrap();
        let socket_path = temp_dir.path().join("test.sock");
        let keys: HashMap<String, SSHCredentials> = HashMap::new();
        let server = SshAgentServer::start(socket_path.clone(), keys).await;
        assert!(server.is_ok());
        drop(server);
    }

    #[tokio::test]
    async fn test_agent_server_creates_missing_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        // Workers mount the socket's parent directory, so the server must be
        // able to create it when only the run-dir root exists.
        let socket_path = temp_dir.path().join("ssh-agent/ssh-agent.sock");
        assert!(!socket_path.parent().unwrap().exists());
        let keys: HashMap<String, SSHCredentials> = HashMap::new();
        let server = SshAgentServer::start(socket_path.clone(), keys).await;
        assert!(server.is_ok());
        assert!(socket_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(socket_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o777, 0o755, "socket dir must be traversable");
        }
        drop(server);
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn test_agent_rejects_valid_rsa_key() {
        let agent = SshAgent::new();
        let result = agent
            .add_key("rsa.example".to_string(), rsa_test_credentials())
            .await;
        assert!(
            matches!(result, Err(SshBridgeError::InvalidKeyFormat(_))),
            "got {result:?}"
        );
        assert!(agent.list_keys().await.is_empty());
    }
}
