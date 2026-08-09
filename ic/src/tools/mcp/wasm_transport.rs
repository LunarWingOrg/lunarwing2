//! Stateful MCP-over-WASM transport.
//!
//! Each active server owns one persistent component instance. Requests are
//! serialized through that actor so guest memory can retain negotiated session
//! state, subscriptions, and server-owned resources. The JSON-RPC envelope stays
//! opaque to the WIT ABI and is validated again at the host boundary.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use wasmtime::Store;
use wasmtime::component::{Component, Linker};

use crate::tools::mcp::protocol::{McpRequest, McpResponse};
use crate::tools::mcp::transport::McpTransport;
use crate::tools::tool::ToolError;
use crate::tools::wasm::{
    EPOCH_TICK_INTERVAL, WasmResourceLimiter, WasmRuntimeConfig, WasmToolRuntime,
};

const MAX_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

wasmtime::component::bindgen!({
    path: "wit/mcp-server.wit",
    world: "mcp-server",
    with: {},
});

struct WasmMcpStoreData {
    limiter: WasmResourceLimiter,
}

impl WasmMcpStoreData {
    fn new(memory_bytes: u64) -> Self {
        Self {
            limiter: WasmResourceLimiter::new(memory_bytes),
        }
    }
}

struct WasmMcpActor {
    store: Store<WasmMcpStoreData>,
    instance: McpServer,
}

/// Persistent MCP component transport with bounded memory, CPU, time, and I/O.
pub struct WasmMcpTransport {
    server_name: String,
    actor: Arc<Mutex<Option<WasmMcpActor>>>,
    fuel: u64,
    timeout: Duration,
    _runtime: Arc<WasmToolRuntime>,
}

impl WasmMcpTransport {
    pub async fn load(
        server_name: &str,
        component_path: &Path,
        memory_mb: u64,
        fuel: u64,
        timeout_seconds: u64,
        expected_component_sha256: Option<String>,
    ) -> Result<Self, ToolError> {
        let wasm_bytes = read_component_bytes(server_name, component_path).await?;
        verify_component_digest(
            server_name,
            &wasm_bytes,
            expected_component_sha256.as_deref(),
        )?;
        let memory_bytes = memory_mb.checked_mul(1024 * 1024).ok_or_else(|| {
            ToolError::InvalidParameters("MCP-over-WASM memory limit overflow".to_string())
        })?;
        let runtime = shared_wasm_runtime()?;
        let component = compile_component(Arc::clone(&runtime), wasm_bytes).await?;
        let actor = instantiate_actor(
            Arc::clone(&runtime),
            component,
            memory_bytes,
            fuel,
            timeout_seconds,
        )
        .await?;

        Ok(Self {
            server_name: server_name.to_string(),
            actor: Arc::new(Mutex::new(Some(actor))),
            fuel,
            timeout: Duration::from_secs(timeout_seconds),
            _runtime: runtime,
        })
    }

    fn epoch_ticks(&self) -> u64 {
        epoch_ticks(self.timeout)
    }
}

fn verify_component_digest(
    server_name: &str,
    bytes: &[u8],
    expected: Option<&str>,
) -> Result<(), ToolError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(ToolError::ExternalService(format!(
            "MCP-over-WASM component digest changed while loading '{server_name}'"
        )));
    }
    Ok(())
}

#[async_trait]
impl McpTransport for WasmMcpTransport {
    async fn send(
        &self,
        request: &McpRequest,
        _headers: &HashMap<String, String>,
    ) -> Result<McpResponse, ToolError> {
        let request_json = serde_json::to_string(request).map_err(|error| {
            ToolError::ExternalService(format!("Failed to serialize MCP request: {error}"))
        })?;
        if request_json.len() > MAX_MESSAGE_BYTES {
            return Err(ToolError::InvalidParameters(format!(
                "MCP-over-WASM request exceeds {} bytes",
                MAX_MESSAGE_BYTES
            )));
        }

        let actor = Arc::clone(&self.actor);
        let server_name = self.server_name.clone();
        let fuel = self.fuel;
        let epoch_ticks = self.epoch_ticks().max(1);
        let response_json = tokio::task::spawn_blocking(move || {
            call_actor(&actor, &server_name, &request_json, fuel, epoch_ticks)
        })
        .await
        .map_err(|error| {
            ToolError::ExternalService(format!(
                "MCP-over-WASM execution task failed for '{}': {error}",
                self.server_name
            ))
        })??;

        if request.id.is_none() && response_json.is_empty() {
            return Ok(empty_notification_response());
        }
        if response_json.len() > MAX_MESSAGE_BYTES {
            return Err(ToolError::ExternalService(format!(
                "MCP-over-WASM response exceeds {} bytes",
                MAX_MESSAGE_BYTES
            )));
        }
        validate_response(&self.server_name, request, &response_json)
    }

    async fn shutdown(&self) -> Result<(), ToolError> {
        let actor = Arc::clone(&self.actor);
        tokio::task::spawn_blocking(move || {
            let mut guard = actor.lock().map_err(|_| {
                ToolError::ExternalService("MCP-over-WASM actor lock is poisoned".to_string())
            })?;
            *guard = None;
            Ok(())
        })
        .await
        .map_err(|error| {
            ToolError::ExternalService(format!("MCP-over-WASM shutdown task failed: {error}"))
        })?
    }

    fn is_healthy(&self) -> bool {
        self.actor
            .lock()
            .map(|actor| actor.is_some())
            .unwrap_or(false)
    }
}

fn shared_wasm_runtime() -> Result<Arc<WasmToolRuntime>, ToolError> {
    static RUNTIME: OnceLock<Result<Arc<WasmToolRuntime>, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            WasmToolRuntime::new(WasmRuntimeConfig::default())
                .map(Arc::new)
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map(Arc::clone)
        .map_err(|error| {
            ToolError::ExternalService(format!("Failed to initialize MCP WASM runtime: {error}"))
        })
}

async fn compile_component(
    runtime: Arc<WasmToolRuntime>,
    bytes: Vec<u8>,
) -> Result<Component, ToolError> {
    tokio::task::spawn_blocking(move || {
        Component::new(runtime.engine(), bytes).map_err(|error| {
            ToolError::ExternalService(format!("Failed to compile MCP WASM component: {error}"))
        })
    })
    .await
    .map_err(|error| {
        ToolError::ExternalService(format!("MCP WASM compilation task failed: {error}"))
    })?
}

async fn instantiate_actor(
    runtime: Arc<WasmToolRuntime>,
    component: Component,
    memory_bytes: u64,
    fuel: u64,
    timeout_seconds: u64,
) -> Result<WasmMcpActor, ToolError> {
    tokio::task::spawn_blocking(move || {
        instantiate_actor_sync(&runtime, &component, memory_bytes, fuel, timeout_seconds)
    })
    .await
    .map_err(|error| {
        ToolError::ExternalService(format!("MCP WASM instantiation task failed: {error}"))
    })?
}

fn instantiate_actor_sync(
    runtime: &WasmToolRuntime,
    component: &Component,
    memory_bytes: u64,
    fuel: u64,
    timeout_seconds: u64,
) -> Result<WasmMcpActor, ToolError> {
    // This world imports no WASI interfaces. An empty linker prevents guests
    // from parking inside blocking host calls, so fuel and epoch interruption
    // bound every guest execution path.
    let linker = Linker::new(runtime.engine());
    let mut store = Store::new(runtime.engine(), WasmMcpStoreData::new(memory_bytes));
    store
        .set_fuel(fuel)
        .map_err(|error| ToolError::ExternalService(format!("Failed to set WASM fuel: {error}")))?;
    store.set_epoch_deadline(epoch_ticks(Duration::from_secs(timeout_seconds)));
    store.limiter(|state| &mut state.limiter);
    let instance = McpServer::instantiate(&mut store, component, &linker).map_err(|error| {
        ToolError::ExternalService(format!("Failed to instantiate MCP WASM component: {error}"))
    })?;
    Ok(WasmMcpActor { store, instance })
}

fn epoch_ticks(timeout: Duration) -> u64 {
    let timeout_ms = timeout.as_millis() as u64;
    let tick_ms = EPOCH_TICK_INTERVAL.as_millis() as u64;
    timeout_ms
        .saturating_add(tick_ms.saturating_sub(1))
        .checked_div(tick_ms.max(1))
        .unwrap_or(1)
        .max(1)
}

async fn read_component_bytes(
    server_name: &str,
    component_path: &Path,
) -> Result<Vec<u8>, ToolError> {
    let server_name = server_name.to_string();
    let task_server_name = server_name.clone();
    let component_path = component_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        read_component_bytes_sync(&task_server_name, &component_path)
    })
    .await
    .map_err(|error| {
        ToolError::ExternalService(format!(
            "MCP WASM component read task failed for '{server_name}': {error}"
        ))
    })?
}

fn read_component_bytes_sync(
    server_name: &str,
    component_path: &PathBuf,
) -> Result<Vec<u8>, ToolError> {
    let link_metadata = std::fs::symlink_metadata(component_path)
        .map_err(|error| load_error(server_name, error))?;
    if link_metadata.file_type().is_symlink() {
        return Err(ToolError::InvalidParameters(format!(
            "MCP-over-WASM component must not be a symlink: {}",
            component_path.display()
        )));
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(component_path)
        .map_err(|error| load_error(server_name, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| load_error(server_name, error))?;
    if !metadata.is_file() || metadata.len() > MAX_COMPONENT_BYTES {
        return Err(ToolError::InvalidParameters(format!(
            "MCP-over-WASM component must be a regular file no larger than {MAX_COMPONENT_BYTES} bytes"
        )));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_COMPONENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| load_error(server_name, error))?;
    if bytes.len() as u64 > MAX_COMPONENT_BYTES {
        return Err(ToolError::InvalidParameters(format!(
            "MCP-over-WASM component exceeds {MAX_COMPONENT_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

fn validate_response(
    server_name: &str,
    request: &McpRequest,
    response_json: &str,
) -> Result<McpResponse, ToolError> {
    if request.id.is_none() {
        if response_json.is_empty() {
            return Ok(empty_notification_response());
        }
        return Err(ToolError::ExternalService(format!(
            "MCP-over-WASM server '{server_name}' returned a response to a notification"
        )));
    }
    let response: McpResponse = serde_json::from_str(response_json).map_err(|error| {
        ToolError::ExternalService(format!(
            "MCP-over-WASM server '{server_name}' returned invalid JSON-RPC: {error}"
        ))
    })?;
    if response.jsonrpc != "2.0" {
        return Err(ToolError::ExternalService(format!(
            "MCP-over-WASM server '{server_name}' returned unsupported JSON-RPC version"
        )));
    }
    if response.id != request.id {
        return Err(ToolError::ExternalService(format!(
            "MCP-over-WASM server '{server_name}' returned a mismatched response id"
        )));
    }
    if response.result.is_some() == response.error.is_some() {
        return Err(ToolError::ExternalService(format!(
            "MCP-over-WASM server '{server_name}' must return exactly one of result or error"
        )));
    }
    Ok(response)
}

fn call_actor(
    actor: &Mutex<Option<WasmMcpActor>>,
    server_name: &str,
    request_json: &str,
    fuel: u64,
    epoch_ticks: u64,
) -> Result<String, ToolError> {
    let mut guard = actor.lock().map_err(|_| {
        ToolError::ExternalService(format!(
            "MCP-over-WASM actor lock is poisoned for '{server_name}'"
        ))
    })?;
    let Some(actor) = guard.as_mut() else {
        return Err(ToolError::ExternalService(format!(
            "MCP-over-WASM server '{server_name}' is shut down"
        )));
    };
    actor.store.set_fuel(fuel).map_err(|error| {
        ToolError::ExternalService(format!("Failed to replenish MCP WASM fuel: {error}"))
    })?;
    actor.store.set_epoch_deadline(epoch_ticks);
    let result = actor
        .instance
        .lunarwing_mcp_server()
        .call_handle(&mut actor.store, request_json);
    match result {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(message)) => Err(ToolError::ExternalService(format!(
            "MCP-over-WASM server '{server_name}' rejected a request: {message}"
        ))),
        Err(error) => {
            *guard = None;
            Err(ToolError::ExternalService(format!(
                "MCP-over-WASM server '{server_name}' trapped and was stopped: {error}"
            )))
        }
    }
}

fn load_error(server_name: &str, error: std::io::Error) -> ToolError {
    ToolError::ExternalService(format!(
        "Failed to load MCP-over-WASM server '{server_name}': {error}"
    ))
}

fn empty_notification_response() -> McpResponse {
    McpResponse {
        jsonrpc: "2.0".to_string(),
        id: None,
        result: None,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: Option<u64>) -> McpRequest {
        McpRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "tools/list".to_string(),
            params: None,
        }
    }

    #[test]
    fn notification_response_is_empty_success() {
        let response = empty_notification_response();
        assert_eq!(response.jsonrpc, "2.0");
        assert!(response.id.is_none());
        assert!(response.result.is_none());
        assert!(response.error.is_none());
    }

    #[test]
    fn runtime_engine_is_shared_across_servers() {
        let first = shared_wasm_runtime().expect("first runtime");
        let second = shared_wasm_runtime().expect("second runtime");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn epoch_deadline_rounds_up() {
        let runtime = shared_wasm_runtime().expect("test runtime");
        let transport = WasmMcpTransport {
            server_name: "test".to_string(),
            actor: Arc::new(Mutex::new(None)),
            fuel: 100,
            timeout: EPOCH_TICK_INTERVAL + Duration::from_millis(1),
            _runtime: runtime,
        };
        assert_eq!(transport.epoch_ticks(), 2);
    }

    #[test]
    fn component_digest_verification_fails_closed() {
        let digest = format!("{:x}", Sha256::digest(b"component"));
        verify_component_digest("test", b"component", Some(&digest)).expect("matching digest");
        assert!(verify_component_digest("test", b"replacement", Some(&digest)).is_err());
        verify_component_digest("test", b"unverified", None).expect("no pin");
    }

    #[test]
    fn validates_guest_jsonrpc_response_boundary() {
        let rpc_request = request(Some(7));
        let valid = validate_response(
            "test",
            &rpc_request,
            r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#,
        )
        .expect("valid response");
        assert_eq!(valid.id, Some(7));

        for invalid in [
            r#"{"jsonrpc":"1.0","id":7,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":8,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":7,"result":{},"error":{"code":-1,"message":"bad"}}"#,
            r#"{"jsonrpc":"2.0","id":7}"#,
        ] {
            assert!(validate_response("test", &rpc_request, invalid).is_err());
        }
        assert!(
            validate_response(
                "test",
                &request(None),
                r#"{"jsonrpc":"2.0","id":null,"result":{}}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn component_reader_rejects_oversized_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let component = temp.path().join("oversized.wasm");
        let file = std::fs::File::create(&component).expect("create sparse fixture");
        file.set_len(MAX_COMPONENT_BYTES + 1)
            .expect("size sparse fixture");

        let error = read_component_bytes_sync("test", &component)
            .expect_err("oversized component must fail");
        assert!(error.to_string().contains("no larger"));
    }

    #[cfg(unix)]
    #[test]
    fn component_reader_rejects_final_symlink_but_allows_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir");
        let real_dir = temp.path().join("real");
        std::fs::create_dir(&real_dir).expect("real dir");
        let component = real_dir.join("server.wasm");
        std::fs::write(&component, b"component-bytes").expect("component fixture");

        let final_link = temp.path().join("server-link.wasm");
        symlink(&component, &final_link).expect("final symlink");
        assert!(read_component_bytes_sync("test", &final_link).is_err());

        let parent_link = temp.path().join("linked-parent");
        symlink(&real_dir, &parent_link).expect("parent symlink");
        let bytes = read_component_bytes_sync("test", &parent_link.join("server.wasm"))
            .expect("symlinked parent is safe when final file is opened without following links");
        assert_eq!(bytes, b"component-bytes");
    }
}
