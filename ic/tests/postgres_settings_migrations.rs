#![cfg(all(feature = "postgres", feature = "integration"))]
//! V28 data selectivity against PostgreSQL's real JSONB semantics.

#[tokio::test]
async fn secret_delete_grant_cleanup_is_all_owner_and_idempotent() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not configured for PostgreSQL integration tests");
        return;
    };
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .expect("connect to PostgreSQL integration database");
    let connection_task = tokio::spawn(connection);
    // A session-local table shadows public.settings. Never modify real users
    // or the migration ledger, even when sharing the CI integration database.
    client
        .batch_execute(
            "CREATE TEMP TABLE settings (
            user_id TEXT NOT NULL, key TEXT NOT NULL, value JSONB NOT NULL,
            PRIMARY KEY (user_id, key)
        );
        INSERT INTO settings VALUES
            ('alice', 'tool_permissions.secret_delete', '\"always_allow\"'),
            ('bob', 'tool_permissions.secret_delete', ' \"always_allow\" '),
            ('carol', 'tool_permissions.secret_delete', '\"disabled\"'),
            ('dave', 'tool_permissions.secret_delete', '\"ask_each_time\"'),
            ('alice', 'tool_permissions.secret_list', '\"always_allow\"'),
            ('bob', 'tool_permissions.ssh', '\"always_allow\"'),
            ('eve', 'tool_permissions.secret_delete', '{\"nested\":\"always_allow\"}');",
        )
        .await
        .unwrap();
    let sql = include_str!("../migrations/V28__revoke_secret_delete_always_grants.sql");
    for _ in 0..2 {
        client.batch_execute(sql).await.unwrap();
        let rows = client
            .query(
                "SELECT user_id, key, value FROM settings ORDER BY user_id, key",
                &[],
            )
            .await
            .unwrap();
        let saved: Vec<(String, String, serde_json::Value)> = rows
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2)))
            .collect();
        assert_eq!(
            saved,
            vec![
                (
                    "alice".into(),
                    "tool_permissions.secret_list".into(),
                    serde_json::json!("always_allow")
                ),
                (
                    "bob".into(),
                    "tool_permissions.ssh".into(),
                    serde_json::json!("always_allow")
                ),
                (
                    "carol".into(),
                    "tool_permissions.secret_delete".into(),
                    serde_json::json!("disabled")
                ),
                (
                    "dave".into(),
                    "tool_permissions.secret_delete".into(),
                    serde_json::json!("ask_each_time")
                ),
                (
                    "eve".into(),
                    "tool_permissions.secret_delete".into(),
                    serde_json::json!({"nested": "always_allow"})
                ),
            ]
        );
    }
    drop(client);
    connection_task.await.unwrap().unwrap();
}
