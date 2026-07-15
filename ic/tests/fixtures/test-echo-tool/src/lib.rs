//! Test-only WASM component for Engine V2 compatibility testing.
//!
//! Exports two deterministic actions:
//! - `echo`: returns `{"source":"engine-v2-test-wasm","echo":<input>}`.
//! - `denied_http_probe`: attempts an HTTP request via the WIT host import
//!   while the capabilities file declares an empty HTTP allowlist, proving
//!   the host rejects undeclared endpoints.

wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "wit/tool.wit",
});

use exports::lunarwing::agent::tool;

const SOURCE_MARKER: &str = "engine-v2-test-wasm";

struct EchoTool;

export!(EchoTool);

impl tool::Guest for EchoTool {
    fn execute(req: tool::Request) -> tool::Response {
        let params = req.params.as_str();
        let result = dispatch(params);
        match result {
            Ok(output) => tool::Response {
                output: Some(output),
                error: None,
            },
            Err(e) => tool::Response {
                output: None,
                error: Some(e),
            },
        }
    }

    fn schema() -> String {
        r#"{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "enum": ["echo", "denied_http_probe"],
      "description": "Which action to perform."
    },
    "message": {
      "type": "string",
      "description": "Input text for echo."
    }
  },
  "required": ["action"]
}"#
        .to_string()
    }

    fn description() -> String {
        "Engine V2 compatibility test tool. Actions: echo (returns source marker + input), denied_http_probe (attempts blocked HTTP to verify capability denial).".to_string()
    }
}

fn dispatch(params_json: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Action {
        Echo,
        DeniedHttpProbe,
    }

    #[derive(serde::Deserialize)]
    struct Params {
        action: Action,
        message: Option<String>,
    }

    let params: Params = serde_json::from_str(params_json)
        .map_err(|e| format!("invalid params: {e}"))?;

    match params.action {
        Action::Echo => {
            let msg = params.message.unwrap_or_default();
            Ok(serde_json::json!({
                "source": SOURCE_MARKER,
                "echo": msg,
            })
            .to_string())
        }
        Action::DeniedHttpProbe => {
            // Attempt an HTTP request. The capabilities file declares no
            // HTTP allowlist, so the host must reject this before any
            // network call occurs.
            let headers = serde_json::json!({"Content-Type": "application/json"});
            let result = lunarwing::agent::host::http_request(
                "GET",
                "https://example.test/should-never-connect",
                &headers.to_string(),
                None,
                Some(5000),
            );
            match result {
                Ok(resp) => Err(format!(
                    "unexpected success: HTTP {} (connection should have been denied)",
                    resp.status
                )),
                Err(e) => Err(format!("denied: {e}")),
            }
        }
    }
}
