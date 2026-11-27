use std::time::Duration;

use tokio::process::Command;

#[tokio::test]
async fn gateway_user_tokens_are_rejected_by_daemon_startup() {
    let instance = tempfile::tempdir().expect("temporary LunarWing instance");
    std::fs::write(instance.path().join(".env"), "").expect("isolated dotenv");
    let map = r#"{"test-user-bearer":{"user_id":"another-user"}}"#;

    for (enabled, owner_token, user_tokens) in [
        ("true", None, map),
        ("true", Some("test-owner-bearer"), map),
        ("false", None, "test-user-bearer-invalid-json"),
    ] {
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_lunarwing"));
        daemon
            .args(["--no-onboard", "--no-db", "run"])
            .current_dir(instance.path())
            .env_clear()
            .env("HOME", instance.path())
            .env("LUNARWING_BASE_DIR", instance.path())
            .env("GATEWAY_ENABLED", enabled)
            .env("GATEWAY_USER_TOKENS", user_tokens)
            .kill_on_drop(true);
        if let Some(token) = owner_token {
            daemon.env("GATEWAY_AUTH_TOKEN", token);
        }

        let output = tokio::time::timeout(Duration::from_secs(15), daemon.output())
            .await
            .expect("daemon exits during configuration")
            .expect("run the LunarWing daemon");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        for secret in ["test-user-bearer", "test-owner-bearer"] {
            assert!(!stderr.contains(secret) && !stdout.contains(secret));
        }
        assert!(!output.status.success());
        assert!(
            stderr.contains("Invalid configuration value for GATEWAY_USER_TOKENS"),
            "unexpected startup error: {stderr}"
        );
        assert!(stderr.contains("unset GATEWAY_USER_TOKENS"));
    }
}
