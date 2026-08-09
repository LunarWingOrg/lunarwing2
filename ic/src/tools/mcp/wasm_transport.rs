//! Stateful MCP-over-WASM transport.
//!
//! Each active server owns one persistent component instance. Requests are
//! serialized through that actor so guest memory can retain negotiated session
//! state, subscriptions, and server-owned resources. The JSON-RPC envelope stays
//! opaque to the WIT ABI and is validated again at the host boundary.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use wasmtime::Store;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::tools::mcp::protocol::{McpRequest, McpResponse};
use crate::tools::mcp::transport::McpTransport;
use crate::tools::tool::ToolError;
use crate::tools::wasm::{WasmResourceLimiter, WasmRuntimeConfig, WasmToolRuntime};

const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(500);
const MAX_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

wasmtime::component::bindgen!({
    path: "wit/mcp-server.wit",
    world: "mcp-server",
    with: {},
});

struct WasmMcpStoreData {
    limiter: WasmResourceLimiter,
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasmMcpStoreData {
    fn new(memory_bytes: u64) -> Self {
        Self {
            limiter: WasmResourceLimiter::new(memory_bytes),
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        }
    }
}

impl WasiView for WasmMcpStoreData {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
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
    ) -> Result<Self, ToolError> {
        let canonical = tokio::fs::canonicalize(component_path)
            .await
            .map_err(|error| load_error(server_name, error))?;
        if canonical != component_path {
            return Err(ToolError::InvalidParameters(format!(
                "MCP-over-WASM component path must be canonical and must not be a symlink: {}",
                component_path.display()
            )));
        }
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(|error| load_error(server_name, error))?;
        if !metadata.is_file() || metadata.len() > MAX_COMPONENT_BYTES {
            return Err(ToolError::InvalidParameters(format!(
                "MCP-over-WASM component must be a regular file no larger than {} bytes",
                MAX_COMPONENT_BYTES
            )));
        }
        let wasm_bytes = tokio::fs::read(&canonical)
            .await
            .map_err(|error| load_error(server_name, error))?;
        let memory_bytes = memory_mb.checked_mul(1024 * 1024).ok_or_else(|| {
            ToolError::InvalidParameters("MCP-over-WASM memory limit overflow".to_string())
        })?;
        let runtime = shared_wasm_runtime()?;
        let component = compile_component(Arc::clone(&runtime), wasm_bytes).await?;
        let actor = instantiate_actor(&runtime, &component, memory_bytes, fuel, timeout_seconds)?;

        Ok(Self {
            server_name: server_name.to_string(),
            actor: Arc::new(Mutex::new(Some(actor))),
            fuel,
            timeout: Duration::from_secs(timeout_seconds),
            _runtime: runtime,
        })
    }

    fn epoch_ticks(&self) -> u64 {
        let timeout_ms = self.timeout.as_millis() as u64;
        let tick_ms = EPOCH_TICK_INTERVAL.as_millis() as u64;
        timeout_ms.saturating_add(tick_ms.saturating_sub(1)) / tick_ms.max(1)
    }
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
        let response: McpResponse = serde_json::from_str(&response_json).map_err(|error| {
            ToolError::ExternalService(format!(
                "MCP-over-WASM server '{}' returned invalid JSON-RPC: {error}",
                self.server_name
            ))
        })?;
        if request.id.is_some() && response.id != request.id {
            return Err(ToolError::ExternalService(format!(
                "MCP-over-WASM server '{}' returned a mismatched response id",
                self.server_name
            )));
        }
        Ok(response)
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

fn instantiate_actor(
    runtime: &WasmToolRuntime,
    component: &Component,
    memory_bytes: u64,
    fuel: u64,
    timeout_seconds: u64,
) -> Result<WasmMcpActor, ToolError> {
    let mut linker = Linker::new(runtime.engine());
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|error| {
        ToolError::ExternalService(format!("Failed to link MCP WASM WASI imports: {error}"))
    })?;
    let mut store = Store::new(runtime.engine(), WasmMcpStoreData::new(memory_bytes));
    store
        .set_fuel(fuel)
        .map_err(|error| ToolError::ExternalService(format!("Failed to set WASM fuel: {error}")))?;
    let ticks =
        Duration::from_secs(timeout_seconds).as_millis() / EPOCH_TICK_INTERVAL.as_millis().max(1);
    store.set_epoch_deadline((ticks as u64).max(1));
    store.limiter(|state| &mut state.limiter);
    let instance = McpServer::instantiate(&mut store, component, &linker).map_err(|error| {
        ToolError::ExternalService(format!("Failed to instantiate MCP WASM component: {error}"))
    })?;
    Ok(WasmMcpActor { store, instance })
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
}
