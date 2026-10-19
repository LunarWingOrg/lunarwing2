//! Real-binary pins for `pebble4lunarwing mcp-shim` (EF-5/KL-6 analog):
//! the baked registration names a subcommand the binary serves, stdout
//! carries only JSON-RPC, and without the proxy env the shim advertises
//! zero tools.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

const NEWLINE: u8 = 10;

fn registration() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/settings.json");
    let text = std::fs::read_to_string(path).expect("read config/settings.json");
    let settings: Value = serde_json::from_str(&text).expect("settings.json is JSON");
    settings["mcpServers"]["lunarwing"].clone()
}

fn shim_exchange(envs: &[(&str, &str)], lines: &[&str]) -> (Vec<Value>, String) {
    let entry = registration();
    let args: Vec<String> = entry["args"]
        .as_array()
        .expect("args array")
        .iter()
        .map(|arg| arg.as_str().expect("string arg").to_string())
        .collect();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pebble4lunarwing"));
    cmd.args(&args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn shim");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for line in lines {
            stdin.write_all(line.as_bytes()).expect("write line");
            stdin.write_all(&[NEWLINE]).expect("write newline");
        }
    }
    let out = child.wait_with_output().expect("shim exits at stdin EOF");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let replies = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout must carry only JSON-RPC, got {line:?}: {e}"))
        })
        .collect();
    (replies, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn baked_registration_matches_the_image_layout() {
    let entry = registration();
    assert_eq!(entry["type"], "stdio");
    assert_eq!(entry["command"], "/usr/local/bin/pebble4lunarwing");
    assert_eq!(entry["args"], serde_json::json!(["mcp-shim"]));
    let dockerfile = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Dockerfile"))
        .expect("read Dockerfile");
    for needle in [
        "COPY --from=builder /build/bridge/target/release/pebble4lunarwing /usr/local/bin/pebble4lunarwing",
        "COPY pebble4lunarwing/config/settings.json /etc/lunarwing/pebble/settings.json",
        "PEBBLE_CONFIG_HOME=/etc/lunarwing/pebble",
    ] {
        assert!(dockerfile.contains(needle), "Dockerfile must carry: {needle}");
    }
}

#[test]
fn without_the_proxy_env_the_shim_speaks_only_json_rpc_and_offers_nothing() {
    let (replies, stderr) = shim_exchange(
        &[],
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            "not json",
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
        ],
    );
    assert_eq!(replies.len(), 4, "one reply per request, none for the notification: {replies:?}");
    assert!(replies.iter().all(|reply| reply["jsonrpc"] == "2.0"));
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(replies[1]["result"]["tools"], serde_json::json!([]));
    assert_eq!(replies[2]["error"]["code"], -32700);
    assert_eq!(replies[3]["id"], 3);
    assert!(stderr.contains("advertising zero tools"), "the notice goes to stderr: {stderr}");
}

#[test]
fn an_unreachable_relay_yields_no_tools_and_a_tool_error() {
    let (replies, _) = shim_exchange(
        &[
            ("LUNARWING_TOOL_PROXY_SOCKET", "/nonexistent/lunarwing-proxy.sock"),
            ("LUNARWING_TASK_ID", "task-x"),
        ],
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_read","arguments":{}}}"#,
        ],
    );
    assert_eq!(replies[0]["result"]["tools"], serde_json::json!([]));
    assert_eq!(replies[1]["result"]["isError"], true);
}
