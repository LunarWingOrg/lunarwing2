use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use russh::keys::agent::Constraint;
use russh::keys::agent::client::AgentClient;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::RwLock;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::*;
use crate::bridge::ssh_agent::{SshAgentServer, parse_key};
use crate::bridge::ssh_client;
use crate::bridge::ssh_hostkeys::HostKeyVerifier;
use crate::bridge::ssh_secrets::SshSecretsManager;
use crate::context::JobContext;
use crate::secrets::{InMemorySecretsStore, SecretsCrypto, SecretsStore};
use crate::tools::Tool;
use crate::tools::builtin::{SshGitTool, SshTool};

const STRICT_HOST: &str = "127.0.0.1";
const TOFU_HOST: &str = "localhost";
const TENANT: &str = "ssh-live-test";

#[derive(Deserialize)]
struct LiveConfig {
    user: String,
    port: u16,
    certificate_port: u16,
    host_key: String,
    wrong_host_key: String,
    client_key: PathBuf,
    bare_repo: PathBuf,
    seed_repo: PathBuf,
    work_dir: PathBuf,
}

impl LiveConfig {
    fn load() -> Self {
        let path = std::env::var("LUNARWING_SSH_LIVE_CONFIG")
            .expect("LUNARWING_SSH_LIVE_CONFIG must name the harness config");
        let bytes = std::fs::read(path).expect("read live harness config");
        serde_json::from_slice(&bytes).expect("parse live harness config")
    }

    fn host(
        &self,
        hostname: &str,
        port: u16,
        mode: HostKeyMode,
        known_host_key: Option<String>,
    ) -> SSHHostConfig {
        SSHHostConfig {
            host: hostname.to_string(),
            port,
            user: self.user.clone(),
            key_type: SSHKeyType::Ed25519,
            host_key_mode: mode,
            known_host_key,
            connect_timeout_secs: 10,
            operation_timeout_secs: 30,
            keepalive_interval_secs: 60,
            keepalive_max_misses: 3,
        }
    }

    fn credentials(&self) -> SSHCredentials {
        SSHCredentials {
            key_data: Zeroizing::new(
                std::fs::read(&self.client_key).expect("read generated client key"),
            ),
            passphrase: None,
        }
    }
}

fn test_store() -> Arc<dyn SecretsStore + Send + Sync> {
    Arc::new(InMemorySecretsStore::new(Arc::new(
        SecretsCrypto::new(secrecy::SecretString::from(
            "test-master-key-that-is-at-least-32-bytes-long!",
        ))
        .unwrap(),
    )))
}

async fn live_bridge(config: &LiveConfig) -> Arc<RwLock<SSHBridge>> {
    let store = test_store();
    let secrets = SshSecretsManager::new(Arc::clone(&store), TENANT);
    for hostname in [STRICT_HOST, TOFU_HOST] {
        secrets
            .store_key(hostname, &config.credentials().key_data, None)
            .await
            .unwrap();
    }

    let strict = config.host(
        STRICT_HOST,
        config.port,
        HostKeyMode::Strict,
        Some(config.host_key.clone()),
    );
    let accept_first = config.host(TOFU_HOST, config.port, HostKeyMode::AcceptFirst, None);
    let hosts = HashMap::from([
        (strict.host.clone(), strict),
        (accept_first.host.clone(), accept_first),
    ]);
    let mut bridge = SSHBridge::new(
        Uuid::new_v4(),
        TENANT.to_string(),
        hosts,
        store,
        Arc::new(NullAuditLogger),
    )
    .await
    .unwrap();

    let agent_keys = HashMap::from([
        (STRICT_HOST.to_string(), config.credentials()),
        (TOFU_HOST.to_string(), config.credentials()),
    ]);
    let agent = SshAgentServer::start(config.work_dir.join("agent/ssh-agent.sock"), agent_keys)
        .await
        .unwrap();
    let private_key = parse_key(&config.credentials()).unwrap();
    let mut client = AgentClient::connect_uds(agent.socket_path()).await.unwrap();
    client
        .add_identity(&private_key, &[Constraint::Confirm])
        .await
        .unwrap();
    drop(client);
    bridge.agent_server = Some(agent);
    Arc::new(RwLock::new(bridge))
}

async fn ssh_result(tool: &SshTool, host: &str, command: &str) -> serde_json::Value {
    tool.execute(
        serde_json::json!({"host": host, "command": command}),
        &JobContext::new("ssh-live", "live SSH handshake test"),
    )
    .await
    .unwrap()
    .result
}

async fn exercise_ssh_tool(bridge: Arc<RwLock<SSHBridge>>, config: &LiveConfig) {
    let tool = SshTool::new(Arc::clone(&bridge));
    let zero = ssh_result(
        &tool,
        STRICT_HOST,
        "printf 'strict-stdout\\n'; printf 'strict-stderr\\n' >&2",
    )
    .await;
    assert_eq!(zero["output"], "strict-stdout\n");
    assert_eq!(zero["stderr"], "strict-stderr\n");
    assert_eq!(zero["exit_code"], 0);
    assert_eq!(zero["success"], true);

    let nonzero = ssh_result(
        &tool,
        TOFU_HOST,
        "printf 'nonzero-stdout\\n'; printf 'nonzero-stderr\\n' >&2; exit 23",
    )
    .await;
    assert_eq!(nonzero["output"], "nonzero-stdout\n");
    assert_eq!(nonzero["stderr"], "nonzero-stderr\n");
    assert_eq!(nonzero["exit_code"], 23);
    assert_eq!(nonzero["success"], false);

    let missing = "lunarwing-live-command-that-does-not-exist";
    let bad = ssh_result(&tool, STRICT_HOST, missing).await;
    assert_eq!(bad["exit_code"], 127);
    assert_eq!(bad["success"], false);
    assert!(bad["stderr"].as_str().unwrap().contains(missing));

    let verifier = bridge.read().await.host_key_verifier();
    assert!(
        verifier
            .get_host_key(TOFU_HOST, config.port)
            .await
            .is_some(),
        "AcceptFirst must persist the learned host key"
    );
}

async fn exercise_refusals(config: &LiveConfig) {
    // ssh_client.rs unit-tests the certificate callback refusal branch separately.
    let wrong_pin = config.host(
        STRICT_HOST,
        config.port,
        HostKeyMode::Strict,
        Some(config.wrong_host_key.clone()),
    );
    let mismatch = ssh_client::connect_and_exec(
        &wrong_pin,
        &config.credentials(),
        Arc::new(HostKeyVerifier::new()),
        "true",
    )
    .await
    .unwrap_err();
    match &mismatch {
        SshBridgeError::ConnectionRefused(message) => {
            assert!(
                message.contains("Host key verification failed"),
                "{message}"
            );
        }
        other => panic!("expected ConnectionRefused, got {other:?}"),
    }

    let certificate = config.host(
        STRICT_HOST,
        config.certificate_port,
        HostKeyMode::Strict,
        Some(config.host_key.clone()),
    );
    let refused = ssh_client::connect_and_exec(
        &certificate,
        &config.credentials(),
        Arc::new(HostKeyVerifier::new()),
        "true",
    )
    .await
    .unwrap_err();
    match &refused {
        SshBridgeError::ConnectionRefused(message) => {
            assert!(message.contains("No common Key algorithm"), "{message}");
            assert!(
                message.contains("ssh-ed25519-cert-v01@openssh.com"),
                "{message}"
            );
        }
        other => panic!("expected ConnectionRefused, got {other:?}"),
    }
}

fn scrubbed_command(program: &str, home: &Path) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    command.env("HOME", home);
    command.env("LC_ALL", "C");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn exercise_agent(config: &LiveConfig) {
    let socket = config.work_dir.join("agent/ssh-agent.sock");
    let listed = scrubbed_command("ssh-add", &config.work_dir)
        .env("SSH_AUTH_SOCK", &socket)
        .arg("-l")
        .output()
        .await
        .unwrap();
    assert_success(&listed);
    assert!(String::from_utf8_lossy(&listed.stdout).contains("ED25519"));

    let known_hosts = config.work_dir.join("known_hosts");
    std::fs::write(
        &known_hosts,
        format!("[{STRICT_HOST}]:{} {}\n", config.port, config.host_key),
    )
    .unwrap();
    let signed = scrubbed_command("ssh", &config.work_dir)
        .env("SSH_AUTH_SOCK", &socket)
        .args(["-F", "/dev/null", "-T", "-p"])
        .arg(config.port.to_string())
        .arg("-i")
        .arg(config.client_key.with_extension("pub"))
        .args([
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
        ])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        .arg(format!("{}@{STRICT_HOST}", config.user))
        .arg("printf 'agent-signature-ok\\n'")
        .output()
        .await
        .unwrap();
    assert_success(&signed);
    assert_eq!(signed.stdout, b"agent-signature-ok\n");
}

async fn git_stdout(config: &LiveConfig, repo: &Path, args: &[&str]) -> String {
    let output = scrubbed_command("git", &config.work_dir)
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .await
        .unwrap();
    assert_success(&output);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

async fn exercise_ssh_git(bridge: Arc<RwLock<SSHBridge>>, config: &LiveConfig) {
    let verifier = bridge.read().await.host_key_verifier();
    assert!(
        verifier
            .get_host_key(TOFU_HOST, config.port)
            .await
            .is_none(),
        "ssh_git preflight requires an unpinned AcceptFirst host"
    );
    let tool = SshGitTool::new(bridge, config.work_dir.clone());
    let ctx = JobContext::new("ssh-git-live", "live SSH git test");
    let cloned = tool
        .execute(
            serde_json::json!({
                "operation": "clone",
                "host": TOFU_HOST,
                "repo": config.bare_repo,
                "path": "clone",
            }),
            &ctx,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(cloned["exit_code"], 0);
    assert_eq!(cloned["success"], true);
    assert!(
        verifier
            .get_host_key(TOFU_HOST, config.port)
            .await
            .is_some(),
        "ssh_git preflight must persist the AcceptFirst host key"
    );

    let initial = git_stdout(config, &config.seed_repo, &["rev-parse", "HEAD"]).await;
    git_stdout(
        config,
        &config.seed_repo,
        &["commit", "--allow-empty", "-q", "-m", "live fetch"],
    )
    .await;
    git_stdout(config, &config.seed_repo, &["push", "-q", "origin", "main"]).await;
    let expected = git_stdout(config, &config.seed_repo, &["rev-parse", "HEAD"]).await;
    assert_ne!(initial, expected);

    let fetched = tool
        .execute(
            serde_json::json!({
                "operation": "fetch",
                "host": TOFU_HOST,
                "path": "clone",
            }),
            &ctx,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(fetched["exit_code"], 0);
    assert_eq!(fetched["success"], true);

    let clone_path = config.work_dir.join("ssh-git/clone");
    let actual = git_stdout(config, &clone_path, &["rev-parse", "origin/main"]).await;
    assert_eq!(actual, expected, "fetch must update origin/main");
}

#[tokio::test]
#[ignore = "manual live test; run scripts/live-tests/ssh-live.sh"]
async fn live_ssh_tool_end_to_end() {
    let config = LiveConfig::load();
    let bridge = live_bridge(&config).await;
    exercise_ssh_git(Arc::clone(&bridge), &config).await;
    exercise_ssh_tool(Arc::clone(&bridge), &config).await;
    exercise_refusals(&config).await;
    exercise_agent(&config).await;
}
