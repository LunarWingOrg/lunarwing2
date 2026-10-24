//! `tool_proxy.rs` — bridge-side relay for the WI#324 worker tool proxy
//! (pebble lane, ported from `kimicode4lunarwing/scripts/lunarwing_tool_proxy.ts`).
//!
//! Owns a per-task unix-domain socket that the in-task stdio MCP shim
//! (`mcp_shim.rs`, spawned by `pebble` via the registered `mcpServers`
//! entry) connects to, and translates the shim's newline-delimited JSON
//! requests into `tool_list` / `tool_call` frames on the task's hub
//! WebSocket. Replies (`tool_catalog` / `tool_result`) arrive on the same
//! hub connection and are routed back to the waiting shim by call id.
//!
//! IPC request shapes (one JSON object per line):
//!   `{"op": "list"}`
//!   `{"op": "call", "call_id": "<ident>", "name": "<tool>", "arguments": {...}}`
//! IPC response shapes (one line, then the relay half-closes):
//!   `{"ok": true, "tools": [...]}`                    (list)
//!   `{"ok": true, "status", "output", "error"}`       (call)
//!   `{"ok": false, "error": "<reason>"}`              (any failure)
//!
//! Bounds mirror the hub (`external_worker.rs`): a serialized `tool_call`
//! frame must fit 256 KiB; name/`call_id` are `[A-Za-z0-9_-]{1,128}`. The
//! relay pre-checks the frame size so an oversized call gets a clean local
//! error instead of a hub-side frame rejection that fails the whole job.
//!
//! One relay exists per hub connection (the pebble bridge serves one daemon
//! connection at a time per accepted socket), so the kimi lane's
//! `pendingBySocket` map collapses to a single pending table scoped by the
//! connection's lifetime; `drop_hub` on connection close is the EV-4/EF-9
//! equivalent.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::protocol::{self, Envelope};

/// Keep in sync with `MAX_TOOL_CALL_FRAME_BYTES` in
/// `ic/src/orchestrator/external_worker.rs` and `TOOL_CALL_FRAME_CAP_BYTES`
/// in `kimicode4lunarwing/scripts/lunarwing_tool_proxy.ts`.
pub const TOOL_CALL_FRAME_CAP_BYTES: usize = 256 * 1024;
/// IPC line cap: a request line larger than this can never fit the WS frame
/// cap, so the connection is told off and the line is dropped.
const MAX_IPC_LINE_BYTES: usize = TOOL_CALL_FRAME_CAP_BYTES;
const MAX_IDENT_LEN: usize = 128;
const DEFAULT_PROXY_TIMEOUT_MS: u64 = 300_000;
const MAX_PROXY_TIMEOUT_MS: u64 = 900_000;
const MIN_PROXY_TIMEOUT_MS: u64 = 1_000;

/// Outbound hub frames for one connection, drained by the bridge's writer
/// task. Shared with bridge.rs.
pub(crate) type Outbound = mpsc::UnboundedSender<Message>;

/// Env key pointing the in-task shim at the per-task relay socket.
pub const PROXY_SOCKET_ENV: &str = "LUNARWING_TOOL_PROXY_SOCKET";
/// Env key carrying the hub-bound task id into the task process.
pub const TASK_ID_ENV: &str = "LUNARWING_TASK_ID";

fn proxy_timeout_from_env() -> Duration {
    let raw = std::env::var("LUNARWING_TOOL_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let ms = raw.map_or(DEFAULT_PROXY_TIMEOUT_MS, |v| {
        v.clamp(MIN_PROXY_TIMEOUT_MS, MAX_PROXY_TIMEOUT_MS)
    });
    Duration::from_millis(ms)
}

/// Directory the per-task sockets live in. Overridable for tests and for
/// operators via `LUNARWING_TOOL_PROXY_DIR`; kept short because unix socket
/// paths cap at ~108 bytes.
#[must_use]
pub fn tool_proxy_dir() -> PathBuf {
    std::env::var("LUNARWING_TOOL_PROXY_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/tmp/lunarwing-tool-proxy"), PathBuf::from)
}

fn is_valid_ident(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENT_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// FNV-1a 64-bit: a small dependency-free hash for the socket-path suffix.
/// This is NOT a security boundary (the proxy dir is a same-uid shared /tmp
/// tree — EF-1/EF-2); it only keeps paths unique per task id.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Filesystem-safe per-task socket path under `dir`. Task ids come from the
/// hub but are still treated as untrusted input for path construction
/// (sanitized + hash-suffixed).
#[must_use]
pub fn task_socket_path_in(dir: &Path, task_id: &str) -> PathBuf {
    let safe: String = task_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    dir.join(format!("{safe}-{:016x}.sock", fnv1a_64(task_id.as_bytes())))
}

/// Environment the task's MCP shim needs, for injection into the pebble
/// child AFTER task-supplied env so it cannot be shadowed.
#[must_use]
pub fn proxy_env_for_task(task_id: &str, socket_path: &Path) -> [(String, String); 2] {
    [
        (TASK_ID_ENV.to_string(), task_id.to_string()),
        (
            PROXY_SOCKET_ENV.to_string(),
            socket_path.to_string_lossy().into_owned(),
        ),
    ]
}

struct PendingWaiter {
    task_id: String,
    is_catalog: bool,
    reply: oneshot::Sender<Value>,
}

/// Waiters plus the set of tasks whose IPC server is still open. Both live
/// under one lock so a request can never park a waiter for a task that
/// `close_task` has already swept (the kimi lane's `task.closed` gate).
#[derive(Default)]
struct PendingTable {
    waiters: HashMap<String, PendingWaiter>,
    live: HashSet<String>,
}

const TASK_ENDED: &str = "task has ended";
const HUB_CLOSED: &str = "hub connection closed before the tool reply arrived";

struct RelayCore {
    pending: Mutex<PendingTable>,
    /// EV-4: once the hub connection is gone it stays gone — a request
    /// arriving on a still-open task IPC server fails at once instead of
    /// re-creating pending state and waiting out the relay timeout.
    hub_open: AtomicBool,
}

struct TaskProxy {
    socket_path: PathBuf,
    accept_loop: tokio::task::JoinHandle<()>,
}

/// Handle returned to the bridge for one task's relay slot.
pub struct TaskProxyHandle {
    pub socket_path: PathBuf,
}

/// One relay per hub connection. Tracks pending hub replies (the hub's
/// `tool_result`/`tool_catalog` frames carry `call_id` but no `task_id`, so
/// routing is scoped by the connection the reply arrived on) and one IPC
/// server per active task.
pub struct ToolProxyRelay {
    core: Arc<RelayCore>,
    tasks: Mutex<HashMap<String, TaskProxy>>,
    dir: PathBuf,
    timeout: Duration,
}

impl ToolProxyRelay {
    /// Production constructor: directory and timeout from the environment.
    pub fn new() -> Self {
        Self::in_dir(tool_proxy_dir())
    }

    /// A relay whose sockets live in `dir`, with the timeout from the env.
    pub fn in_dir(dir: PathBuf) -> Self {
        Self::with_overrides(dir, proxy_timeout_from_env())
    }

    pub fn with_overrides(dir: PathBuf, timeout: Duration) -> Self {
        Self {
            core: Arc::new(RelayCore {
                pending: Mutex::new(PendingTable::default()),
                hub_open: AtomicBool::new(true),
            }),
            tasks: Mutex::new(HashMap::new()),
            dir,
            timeout,
        }
    }

    /// Start the per-task IPC server. Returns the handle, or None when the
    /// server could not be started (the task then simply runs without
    /// tools). Never fails hard: the filesystem setup runs on a shared,
    /// same-uid /tmp tree that a concurrent task process could sabotage
    /// (MR !1549 EF-2).
    pub async fn start_task(&self, task_id: &str, out_tx: Outbound) -> Option<TaskProxyHandle> {
        let socket_path = task_socket_path_in(&self.dir, task_id);
        let mut tasks = self.tasks.lock().await;
        if tasks.contains_key(task_id) {
            return None;
        }
        if let Err(e) = prepare_socket_path(&socket_path) {
            eprintln!(
                "[tool-proxy] task {task_id}: proxy dir setup failed (task runs without tools): {e}"
            );
            return None;
        }
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!("[tool-proxy] task {task_id}: listen failed: {e}");
                return None;
            }
        };
        // LD-15: hand THIS task's socket to the task identity before the
        // task is spawned (the caller spawns right after this returns).
        hand_socket_to_task(&socket_path, task_id);
        self.core
            .pending
            .lock()
            .await
            .live
            .insert(task_id.to_string());
        let accept_loop = tokio::spawn(accept_loop(
            listener,
            Arc::clone(&self.core),
            out_tx,
            task_id.to_string(),
            self.timeout,
        ));
        tasks.insert(
            task_id.to_string(),
            TaskProxy {
                socket_path: socket_path.clone(),
                accept_loop,
            },
        );
        drop(tasks);
        Some(TaskProxyHandle { socket_path })
    }

    /// Route an inbound hub frame (`tool_result` / `tool_catalog`) to its
    /// waiter. Returns true when the frame was consumed.
    pub async fn handle_hub_frame(&self, envelope: &Envelope) -> bool {
        if !envelope.payload.is_object() {
            return false;
        }
        match envelope.msg_type.as_str() {
            "tool_result" => {
                let call_id = envelope
                    .payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let waiter = {
                    let mut pending = self.core.pending.lock().await;
                    let routable = matches!(pending.waiters.get(call_id), Some(w) if !w.is_catalog);
                    if routable {
                        pending.waiters.remove(call_id)
                    } else {
                        None
                    }
                };
                let Some(waiter) = waiter else { return false };
                let _ = waiter.reply.send(envelope.payload.clone());
                true
            }
            "tool_catalog" => {
                // tool_list carries no call_id; at most one catalog request
                // is in flight per task, keyed synthetically.
                let waiter = {
                    let mut pending = self.core.pending.lock().await;
                    let key = pending
                        .waiters
                        .iter()
                        .find(|(_, waiter)| waiter.is_catalog)
                        .map(|(key, _)| key.clone());
                    key.and_then(|key| pending.waiters.remove(&key))
                };
                let Some(waiter) = waiter else { return false };
                let _ = waiter.reply.send(envelope.payload.clone());
                true
            }
            _ => false,
        }
    }

    /// Close the task's IPC server and fail every call it still has in
    /// flight. Idempotent and never-failing (EF-2): teardown runs from task
    /// cleanup paths that must still release their other slots.
    pub async fn close_task(&self, task_id: &str) {
        let task = self.tasks.lock().await.remove(task_id);
        let Some(task) = task else { return };
        // Stop accepting new connections; live connections end themselves
        // once their (final) response is written — one reply per connection,
        // then half-close — so pending waiters are resolved below and their
        // error responses still reach the shim. A shim's next connect gets
        // ECONNREFUSED from the closed server, surfaced as a tool error.
        task.accept_loop.abort();
        if let Err(e) = std::fs::remove_file(&task.socket_path) {
            // Best effort (EF-2): a sabotaged proxy dir must not fail task
            // teardown — the caller still has to release the workspace claim.
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("[tool-proxy] task {task_id}: socket cleanup failed: {e}");
            }
        }
        // Mark the task ended and sweep its waiters under ONE lock: a request
        // still arriving on a connection accepted before the close is then
        // refused instead of reaching the hub for an ended task.
        let mut pending = self.core.pending.lock().await;
        pending.live.remove(task_id);
        let keys: Vec<String> = pending
            .waiters
            .iter()
            .filter(|(_, waiter)| waiter.task_id == task_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if let Some(waiter) = pending.waiters.remove(&key) {
                let _ = waiter
                    .reply
                    .send(json!({"error": "task ended before the tool reply arrived"}));
            }
        }
    }

    /// Hub connection closed: fail every waiter parked on it and refuse new
    /// ones (MR !1549 EF-9, MR !1561 EV-4). Task IPC servers stay up; their
    /// next request fails at once with the same error.
    pub async fn drop_hub(&self) {
        self.core.hub_open.store(false, Ordering::SeqCst);
        let drained: Vec<PendingWaiter> = {
            let mut pending = self.core.pending.lock().await;
            pending.waiters.drain().map(|(_, waiter)| waiter).collect()
        };
        for waiter in drained {
            let _ = waiter
                .reply
                .send(json!({"error": "hub connection closed before the tool reply arrived"}));
        }
    }

    /// Number of tasks with a live IPC server (test introspection).
    #[cfg(test)]
    pub async fn active_task_count(&self) -> usize {
        self.tasks.lock().await.len()
    }
}

impl Default for ToolProxyRelay {
    fn default() -> Self {
        Self::new()
    }
}

/// mkdir -p the parent dir and remove a stale socket file from a crashed
/// earlier task (the path is hash-bound to this task, so a leftover would
/// make `bind()` fail with `EADDRINUSE`).
///
/// LD-15 (the WI#400 shape): the directory is root-owned `0711` — the task
/// uid (and its MCP shim) must be able to TRAVERSE it to connect(2) its own
/// socket, but must be able neither to list other tasks' sockets nor to
/// write or remove them. A symlinked or foreign-owned directory is refused
/// instead of followed: the relay runs as root, and following a link (or
/// adopting a dir another uid prepared) would let task code aim the relay's
/// filesystem writes wherever it likes.
fn prepare_socket_path(socket_path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = socket_path.parent() {
        if let Ok(meta) = std::fs::symlink_metadata(parent) {
            refuse_foreign_proxy_dir(parent, &meta)?;
        }
        std::fs::create_dir_all(parent)?;
        // create_dir_all silently succeeds on an existing symlink-to-dir:
        // re-check on the created path with lstat semantics.
        let meta = std::fs::symlink_metadata(parent)?;
        refuse_foreign_proxy_dir(parent, &meta)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o711))?;
    }
    match std::fs::remove_file(socket_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// LD-15 proxy-dir gate: refuse a symlinked directory and one owned by a uid
/// other than the bridge's. The relay runs as root, and following a link (or
/// adopting a dir another uid prepared) would let task code aim the relay's
/// filesystem writes wherever it likes.
fn refuse_foreign_proxy_dir(parent: &Path, meta: &std::fs::Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "proxy dir {} is a symlink; refusing to follow it",
                parent.display()
            ),
        ));
    }
    let own_uid = bridge_uid(parent)?;
    if meta.uid() != own_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "proxy dir {} is owned by uid {}; refusing to adopt a foreign directory",
                parent.display(),
                meta.uid()
            ),
        ));
    }
    Ok(())
}

/// This process's uid, learned from a probe file the bridge creates in
/// `dir` (the crate forbids unsafe code, so `libc::geteuid` is not an
/// option). LP-15: the owner of /proc/self reads as 0 for a non-dumpable
/// process, so /proc is not a trustworthy self-id source; a file this
/// process just created carries its fsuid as the owner instead (the
/// technique the LD-8 executor test already uses). The name is
/// pid+counter+nanotime so a task watching the directory cannot pre-plant
/// the probe (`create_new` fails closed on a collision).
pub(crate) fn bridge_uid(dir: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let probe = dir.join(format!(
        ".bridge-uid-probe-{}-{}-{nanos}",
        std::process::id(),
        PROBE_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe)
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot create a uid probe in {}: {error}", dir.display()),
            )
        })?;
    let uid = file.metadata().map(|meta| meta.uid());
    drop(file);
    std::fs::remove_file(&probe).ok();
    uid
}

/// LD-15: under the WI#400 split the task process and its MCP shim run as
/// the task uid with no capabilities, so connecting to the per-task socket
/// requires the socket itself to belong to that uid (mode 0600). The socket
/// is handed over right after bind, BEFORE the task is spawned. With no
/// identity pinned (host dev runs and tests) the socket is left untouched.
/// Never throws (EF-2): a failed handoff leaves the task without tools,
/// not without a bridge.
///
/// LP-13: chmod BEFORE lchown — root changing the mode of a file it still
/// owns needs no `CAP_FOWNER` (only the chown's `CAP_CHOWN` after it). The
/// lchown-first order did need FOWNER and made the handoff depend on a
/// capability the docs had marked droppable.
fn hand_socket_to_task(socket_path: &Path, task_id: &str) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(Some(ids)) = crate::executor::task_ids_from_env() else {
        return;
    };
    let handoff = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .and_then(|()| std::os::unix::fs::lchown(socket_path, Some(ids.0), Some(ids.1)));
    if let Err(e) = handoff {
        eprintln!(
            "[tool-proxy] task {task_id}: socket handoff to uid {} failed (task runs without tools): {e}",
            ids.0
        );
    }
}

async fn accept_loop(
    listener: UnixListener,
    core: Arc<RelayCore>,
    out_tx: Outbound,
    task_id: String,
    timeout: Duration,
) {
    loop {
        let Ok((mut conn, _)) = listener.accept().await else {
            break;
        };
        let conn_core = Arc::clone(&core);
        let conn_out = out_tx.clone();
        let conn_task = task_id.clone();
        tokio::spawn(async move {
            handle_ipc_conn(&mut conn, conn_core, conn_out, conn_task, timeout).await;
        });
    }
}

/// One request per connection, one response, then half-close (the kimi
/// lane's end-after-flush): reply bytes leave the kernel buffer even when
/// the task is closing, and every response path — including the over-cap
/// told-off — ends the connection the same way. An over-cap line is
/// discarded up to its terminating newline before the reply is written, and
/// every reply is followed by a drain to EOF: closing with unread data in
/// the socket receive queue would RST the just-written reply away (the shim
/// never pipelines, but a same-uid task process can).
async fn handle_ipc_conn(
    conn: &mut UnixStream,
    core: Arc<RelayCore>,
    out_tx: Outbound,
    task_id: String,
    timeout: Duration,
) {
    const OVER_CAP: &str = "request line exceeds the 256 KiB frame budget";
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 65_536].into_boxed_slice();
    let mut discarding = false;
    loop {
        if let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            if discarding || pos > MAX_IPC_LINE_BYTES {
                buffer.drain(..=pos);
                respond_and_drain(conn, json!({"ok": false, "error": OVER_CAP})).await;
                return;
            }
            let mut line: Vec<u8> = buffer.drain(..=pos).collect();
            line.pop(); // the newline
            let text = String::from_utf8_lossy(&line).trim().to_string();
            if text.is_empty() {
                continue;
            }
            let response = handle_ipc_line(&core, &out_tx, &task_id, &text, timeout).await;
            respond_and_drain(conn, response).await;
            return;
        }
        if buffer.len() > MAX_IPC_LINE_BYTES {
            // A line this long without a terminator can never succeed at the
            // hub; keep discarding until its newline, then answer.
            discarding = true;
            buffer.clear();
        }
        match conn.read(&mut chunk).await {
            Ok(0) | Err(_) => return, // peer went away with nothing pending
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
}

async fn respond_and_drain(conn: &mut UnixStream, response: Value) {
    if let Ok(mut text) = serde_json::to_string(&response) {
        text.push('\n');
        if conn.write_all(text.as_bytes()).await.is_ok() {
            let _ = conn.shutdown().await;
        }
    }
    let mut sink = vec![0u8; 65_536].into_boxed_slice();
    loop {
        match conn.read(&mut sink).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

async fn handle_ipc_line(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    task_id: &str,
    line: &str,
    timeout: Duration,
) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(_) => return json!({"ok": false, "error": "request is not valid JSON"}),
    };
    let Some(op) = request.get("op").and_then(Value::as_str) else {
        return json!({"ok": false, "error": "request must be an object with an op field"});
    };
    if !core.pending.lock().await.live.contains(task_id) {
        return json!({"ok": false, "error": TASK_ENDED});
    }
    if !core.hub_open.load(Ordering::SeqCst) {
        return json!({"ok": false, "error": HUB_CLOSED});
    }
    match op {
        "list" => handle_list(core, out_tx, task_id, timeout).await,
        "call" => handle_call(core, out_tx, task_id, &request, timeout).await,
        other => {
            let shown: String = other.chars().take(64).collect();
            json!({"ok": false, "error": format!("unknown op {}", shown)})
        }
    }
}

async fn handle_list(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    task_id: &str,
    timeout: Duration,
) -> Value {
    let frame = protocol::create_envelope("tool_list", json!({ "task_id": task_id }));
    let Ok(serialized) = serde_json::to_string(&frame) else {
        return json!({"ok": false, "error": "tool_catalog request failed or timed out"});
    };
    // One catalog request in flight per task, keyed synthetically (the hub's
    // tool_catalog carries no call_id to correlate by).
    let key = format!("catalog:{task_id}");
    match send_and_wait(core, out_tx, key, task_id, true, serialized, timeout).await {
        Some(payload) => {
            let tools = payload.get("tools").cloned().unwrap_or_else(|| json!([]));
            let error = payload.get("error").cloned().unwrap_or(Value::Null);
            json!({"ok": true, "tools": tools, "error": error})
        }
        None => json!({"ok": false, "error": "tool_catalog request failed or timed out"}),
    }
}

async fn handle_call(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    task_id: &str,
    request: &Value,
    timeout: Duration,
) -> Value {
    let Some(call_id) = request.get("call_id").and_then(Value::as_str) else {
        return json!({"ok": false, "error": "call_id must be [A-Za-z0-9_-]{1,128}"});
    };
    if !is_valid_ident(call_id) {
        return json!({"ok": false, "error": "call_id must be [A-Za-z0-9_-]{1,128}"});
    }
    let Some(name) = request.get("name").and_then(Value::as_str) else {
        return json!({"ok": false, "error": "name must be [A-Za-z0-9_-]{1,128}"});
    };
    if !is_valid_ident(name) {
        return json!({"ok": false, "error": "name must be [A-Za-z0-9_-]{1,128}"});
    }
    let arguments = request.get("arguments").cloned().unwrap_or(Value::Null);
    let frame = protocol::create_envelope(
        "tool_call",
        json!({
            "task_id": task_id,
            "call_id": call_id,
            "name": name,
            "arguments": arguments,
        }),
    );
    let serialized = serde_json::to_string(&frame).unwrap_or_default();
    if serialized.len() > TOOL_CALL_FRAME_CAP_BYTES {
        // KL-8/JU-4: pre-check locally so the caller gets a clean budget
        // error instead of a hub-side frame rejection or a bare EPIPE.
        return json!({"ok": false, "error": format!(
            "tool_call frame exceeds the {TOOL_CALL_FRAME_CAP_BYTES}-byte hub budget"
        )});
    }
    match send_and_wait(
        core,
        out_tx,
        call_id.to_string(),
        task_id,
        false,
        serialized,
        timeout,
    )
    .await
    {
        Some(payload) => {
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("error");
            let output = payload.get("output").and_then(Value::as_str).unwrap_or("");
            let error = payload.get("error").cloned().unwrap_or(Value::Null);
            json!({"ok": true, "status": status, "output": output, "error": error})
        }
        None => json!({"ok": false, "error": "tool_result wait failed or timed out"}),
    }
}

/// Park a waiter and send the frame. Resolves `Some(payload)` with the
/// hub's reply payload (including the error payloads synthesized by
/// `close_task` / `drop_hub`), or `None` on duplicate key, send failure, or
/// timeout.
async fn send_and_wait(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    key: String,
    task_id: &str,
    is_catalog: bool,
    serialized: String,
    timeout: Duration,
) -> Option<Value> {
    let (tx, rx) = oneshot::channel();
    {
        let mut pending = core.pending.lock().await;
        // Re-checked under the lock that close_task / drop_hub sweep with,
        // so no waiter can be parked after its sweep ran.
        if !core.hub_open.load(Ordering::SeqCst) {
            return Some(json!({"error": HUB_CLOSED}));
        }
        if !pending.live.contains(task_id) {
            return Some(json!({"error": TASK_ENDED}));
        }
        if pending.waiters.contains_key(&key) {
            // Duplicate call_id in flight; the shim must not reuse ids.
            return None;
        }
        pending.waiters.insert(
            key.clone(),
            PendingWaiter {
                task_id: task_id.to_string(),
                is_catalog,
                reply: tx,
            },
        );
    }
    if out_tx.send(Message::Text(serialized.into())).is_err() {
        core.pending.lock().await.waiters.remove(&key);
        eprintln!("[tool-proxy] hub send failed");
        return None;
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(payload)) => Some(payload),
        Ok(Err(_)) => None, // sender dropped without a resolve
        Err(_) => {
            core.pending.lock().await.waiters.remove(&key);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_dir() -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("pebble-tool-proxy-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create proxy test dir");
        dir
    }

    fn test_relay() -> (ToolProxyRelay, PathBuf) {
        let dir = test_dir();
        (
            ToolProxyRelay::with_overrides(dir.clone(), Duration::from_secs(5)),
            dir,
        )
    }

    fn fake_hub() -> (Outbound, mpsc::UnboundedReceiver<Message>) {
        mpsc::unbounded_channel()
    }

    async fn next_hub_frame(rx: &mut mpsc::UnboundedReceiver<Message>) -> Envelope {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("hub frame within timeout")
            .expect("hub channel open");
        let Message::Text(text) = msg else {
            panic!("expected text frame, got {msg:?}");
        };
        serde_json::from_str(text.as_str()).expect("valid envelope JSON")
    }

    async fn ipc_request(socket_path: &Path, request: &Value) -> Value {
        let mut conn = UnixStream::connect(socket_path)
            .await
            .expect("connect to task socket");
        conn.write_all(serde_json::to_string(request).unwrap().as_bytes())
            .await
            .expect("write request");
        conn.write_all(b"\n").await.expect("write newline");
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut reply))
            .await
            .expect("reply within timeout")
            .expect("read reply");
        let text = String::from_utf8(reply).expect("utf-8 reply");
        serde_json::from_str(text.trim()).expect("reply is JSON")
    }

    #[test]
    fn socket_path_is_sanitized_bounded_and_deterministic() {
        let dir = Path::new("/tmp/x");
        let path = task_socket_path_in(dir, "task/with..bad chars");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(!name.contains('/'));
        assert_eq!(
            Path::new(name).extension().and_then(|e| e.to_str()),
            Some("sock")
        );
        assert!(path.to_str().unwrap().len() <= 108);
        assert_eq!(path, task_socket_path_in(dir, "task/with..bad chars"));
        assert_ne!(path, task_socket_path_in(dir, "task/with..bad chars2"));
        let long = task_socket_path_in(dir, &"a".repeat(500));
        assert!(long.to_str().unwrap().len() <= 108);
    }

    #[test]
    fn ident_bounds_match_the_hub() {
        assert!(is_valid_ident("memory_read"));
        assert!(is_valid_ident("gitlab-tool"));
        assert!(!is_valid_ident(""));
        assert!(!is_valid_ident(&"a".repeat(129)));
        assert!(!is_valid_ident("bad name"));
        assert!(!is_valid_ident("bad/name"));
        assert!(is_valid_ident(&"a".repeat(128)));
    }

    #[tokio::test]
    async fn list_round_trips_through_the_hub() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay
            .start_task("task-list", out_tx)
            .await
            .expect("task starts");

        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move { ipc_request(&socket, &json!({"op": "list"})).await }
        });
        let frame = next_hub_frame(&mut out_rx).await;
        assert_eq!(frame.msg_type, "tool_list");
        assert_eq!(frame.payload["task_id"], "task-list");
        assert!(
            relay
                .handle_hub_frame(&protocol::create_envelope(
                    "tool_catalog",
                    json!({"tools": [{"name": "memory_read"}]}),
                ))
                .await
        );
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["tools"][0]["name"], "memory_read");
        relay.close_task("task-list").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn call_frame_shape_and_result_routing() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-call", out_tx).await.expect("task");

        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-1", "name": "memory_read",
                            "arguments": {"path": "x"}}),
                )
                .await
            }
        });
        let frame = next_hub_frame(&mut out_rx).await;
        assert_eq!(frame.msg_type, "tool_call");
        assert_eq!(frame.payload["task_id"], "task-call");
        assert_eq!(frame.payload["call_id"], "c-1");
        assert_eq!(frame.payload["name"], "memory_read");
        assert_eq!(frame.payload["arguments"]["path"], "x");
        assert!(
            relay.handle_hub_frame(&protocol::create_envelope(
                "tool_result",
                json!({"call_id": "c-1", "status": "success", "output": "value", "error": null}),
            )).await
        );
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "success");
        assert_eq!(reply["output"], "value");
        assert_eq!(reply["error"], Value::Null);
        relay.close_task("task-call").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn denied_status_and_reason_pass_through() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-denied", out_tx).await.expect("task");
        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-2", "name": "shell"}),
                )
                .await
            }
        });
        let frame = next_hub_frame(&mut out_rx).await;
        // Missing arguments defaults to null, matching the wire contract.
        assert_eq!(frame.payload["arguments"], Value::Null);
        assert!(
            relay.handle_hub_frame(&protocol::create_envelope(
                "tool_result",
                json!({"call_id": "c-2", "status": "denied", "output": "", "error": "not in tool_allowlist"}),
            )).await
        );
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "denied");
        assert_eq!(reply["error"], "not in tool_allowlist");
        relay.close_task("task-denied").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn invalid_identifiers_are_rejected_locally() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-ident", out_tx).await.expect("task");
        for request in [
            json!({"op": "call", "call_id": "bad id", "name": "memory_read"}),
            json!({"op": "call", "call_id": "c", "name": "bad/name"}),
            json!({"op": "call", "call_id": "c", "name": "memory_read", "arguments": {"x": "y".repeat(300 * 1024)}}),
            json!({"op": "call", "call_id": 42, "name": "memory_read"}),
        ] {
            let reply = ipc_request(&handle.socket_path, &request).await;
            assert_eq!(reply["ok"], false, "request {request}");
        }
        // Nothing reached the hub.
        assert!(out_rx.try_recv().is_err());
        relay.close_task("task-ident").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn malformed_requests_are_rejected() {
        let (relay, dir) = test_relay();
        let (out_tx, _out_rx) = fake_hub();
        let handle = relay.start_task("task-bad", out_tx).await.expect("task");
        for request in [
            json!("not an object"),
            json!({"nope": 1}),
            json!({"op": "frobnicate"}),
        ] {
            let reply = ipc_request(&handle.socket_path, &request).await;
            assert_eq!(reply["ok"], false, "request {request}");
        }
        relay.close_task("task-bad").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn close_task_fails_in_flight_calls() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-close", out_tx).await.expect("task");
        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-3", "name": "memory_read"}),
                )
                .await
            }
        });
        let _frame = next_hub_frame(&mut out_rx).await;
        relay.close_task("task-close").await;
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["error"], "task ended before the tool reply arrived");
        assert!(!handle.socket_path.exists(), "socket file removed");
        assert_eq!(relay.active_task_count().await, 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn drop_hub_fails_waiters_and_new_requests_fast() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-drop", out_tx).await.expect("task");
        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-4", "name": "memory_read"}),
                )
                .await
            }
        });
        let _frame = next_hub_frame(&mut out_rx).await;
        relay.drop_hub().await;
        let reply = client.await.expect("client joins");
        assert_eq!(
            reply["error"],
            "hub connection closed before the tool reply arrived"
        );
        // EV-4: a later request on the still-open task IPC fails at once.
        let reply = ipc_request(
            &handle.socket_path,
            &json!({"op": "call", "call_id": "c-5", "name": "memory_read"}),
        )
        .await;
        assert_eq!(reply["ok"], false);
        assert_eq!(
            reply["error"],
            "hub connection closed before the tool reply arrived"
        );
        relay.close_task("task-drop").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn duplicate_task_start_is_refused() {
        let (relay, dir) = test_relay();
        let (out_tx, _rx) = fake_hub();
        assert!(relay.start_task("task-dup", out_tx.clone()).await.is_some());
        assert!(relay.start_task("task-dup", out_tx).await.is_none());
        relay.close_task("task-dup").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn duplicate_call_id_and_catalog_requests_are_refused() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay
            .start_task("task-dupcall", out_tx)
            .await
            .expect("task");
        let first = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-6", "name": "memory_read"}),
                )
                .await
            }
        });
        let _frame = next_hub_frame(&mut out_rx).await;
        // Same call_id while one is in flight: refused without a second frame.
        let reply = ipc_request(
            &handle.socket_path,
            &json!({"op": "call", "call_id": "c-6", "name": "memory_read"}),
        )
        .await;
        assert_eq!(reply["ok"], false);
        assert!(out_rx.try_recv().is_err());
        relay.close_task("task-dupcall").await;
        let reply = first.await.expect("first joins");
        assert_eq!(reply["error"], "task ended before the tool reply arrived");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn tool_result_for_an_unknown_call_id_is_unroutable() {
        let (relay, dir) = test_relay();
        assert!(
            !relay
                .handle_hub_frame(&protocol::create_envelope(
                    "tool_result",
                    json!({"call_id": "nobody", "status": "success"}),
                ))
                .await
        );
        assert!(
            !relay
                .handle_hub_frame(&protocol::create_envelope(
                    "tool_catalog",
                    json!({"tools": []}),
                ))
                .await
        );
        assert!(
            !relay
                .handle_hub_frame(&protocol::create_envelope("task_request", json!({})))
                .await
        );
        std::fs::remove_dir_all(dir).ok();
    }

    const NEWLINE: u8 = 10;

    /// LB-1: a connection accepted before `close_task` must not carry a
    /// request to the hub after the task ended (kimi `task.closed` parity).
    #[tokio::test]
    async fn requests_on_connections_accepted_before_close_are_refused() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-late", out_tx).await.expect("task");
        let mut conn = UnixStream::connect(&handle.socket_path)
            .await
            .expect("connect");
        let line = serde_json::to_string(
            &json!({"op": "call", "call_id": "late-1", "name": "memory_read"}),
        )
        .expect("serialize");
        conn.write_all(line.as_bytes()).await.expect("write line");
        // Let the accept loop hand the connection to its handler, which now
        // waits for the terminating newline.
        tokio::time::sleep(Duration::from_millis(250)).await;
        relay.close_task("task-late").await;
        conn.write_all(&[NEWLINE]).await.expect("write newline");
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), conn.read_to_end(&mut reply))
            .await
            .expect("refused at once, not after the relay timeout")
            .expect("read reply");
        let reply: Value =
            serde_json::from_str(String::from_utf8(reply).expect("utf-8").trim()).expect("json");
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "task has ended");
        assert!(out_rx.try_recv().is_err(), "no hub frame for an ended task");
        std::fs::remove_dir_all(dir).ok();
    }

    /// KL-10 analog: closing one task fails only that task's calls, and a
    /// late reply for a call failed by the close is not routed.
    #[tokio::test]
    async fn close_task_fails_only_its_own_calls() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let a = relay.start_task("task-a", out_tx.clone()).await.expect("a");
        let b = relay.start_task("task-b", out_tx).await.expect("b");
        let call = |socket: PathBuf, id: &'static str| {
            tokio::spawn(async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": id, "name": "memory_read"}),
                )
                .await
            })
        };
        let call_a = call(a.socket_path.clone(), "a-1");
        let _ = next_hub_frame(&mut out_rx).await;
        let call_b = call(b.socket_path.clone(), "b-1");
        let _ = next_hub_frame(&mut out_rx).await;
        relay.close_task("task-a").await;
        let reply_a = call_a.await.expect("a joins");
        assert_eq!(reply_a["error"], "task ended before the tool reply arrived");
        let late_a = protocol::create_envelope(
            "tool_result",
            json!({"call_id": "a-1", "status": "success", "output": "late"}),
        );
        assert!(
            !relay.handle_hub_frame(&late_a).await,
            "late reply after close is unroutable"
        );
        let reply_b = protocol::create_envelope(
            "tool_result",
            json!({"call_id": "b-1", "status": "success", "output": "still-b", "error": null}),
        );
        assert!(
            relay.handle_hub_frame(&reply_b).await,
            "the other task's call is still parked"
        );
        assert_eq!(call_b.await.expect("b joins")["output"], "still-b");
        relay.close_task("task-b").await;
        std::fs::remove_dir_all(dir).ok();
    }

    /// The request timeout frees the call id (no pending leak) and a reply
    /// arriving after the timeout is not routed.
    #[tokio::test]
    async fn timed_out_call_frees_its_id_and_ignores_the_late_reply() {
        let dir = test_dir();
        let relay = ToolProxyRelay::with_overrides(dir.clone(), Duration::from_millis(200));
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-slow", out_tx).await.expect("task");
        let request = json!({"op": "call", "call_id": "slow-1", "name": "memory_read"});
        let reply = ipc_request(&handle.socket_path, &request).await;
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "tool_result wait failed or timed out");
        let _ = next_hub_frame(&mut out_rx).await;
        let late = protocol::create_envelope(
            "tool_result",
            json!({"call_id": "slow-1", "status": "success", "output": "late"}),
        );
        assert!(
            !relay.handle_hub_frame(&late).await,
            "late reply after timeout is unroutable"
        );
        // The id is free again: the retry reaches the hub.
        let reply = ipc_request(&handle.socket_path, &request).await;
        assert_eq!(reply["error"], "tool_result wait failed or timed out");
        assert_eq!(
            next_hub_frame(&mut out_rx).await.payload["call_id"],
            "slow-1"
        );
        relay.close_task("task-slow").await;
        std::fs::remove_dir_all(dir).ok();
    }

    /// JU-4/KL-8 analog: the frame budget is checked on the serialized
    /// `tool_call` envelope, not on the IPC line. A line just under the line
    /// cap becomes an over-budget frame and must get the frame-only error;
    /// an over-long line gets the line-cap error. Neither reaches the hub.
    #[tokio::test]
    async fn frame_budget_is_enforced_on_the_frame_not_the_line() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-budget", out_tx).await.expect("task");
        let shaped = |fill: usize| {
            json!({"op": "call", "call_id": "c", "name": "memory_read",
                   "arguments": {"x": "y".repeat(fill)}})
        };
        let base = serde_json::to_string(&shaped(0)).expect("serialize").len();
        let request = shaped(MAX_IPC_LINE_BYTES - 32 - base);
        assert_eq!(
            serde_json::to_string(&request).expect("serialize").len(),
            MAX_IPC_LINE_BYTES - 32
        );
        let reply = ipc_request(&handle.socket_path, &request).await;
        assert_eq!(reply["ok"], false);
        assert_eq!(
            reply["error"],
            format!("tool_call frame exceeds the {TOOL_CALL_FRAME_CAP_BYTES}-byte hub budget")
        );
        let reply = ipc_request(&handle.socket_path, &shaped(MAX_IPC_LINE_BYTES)).await;
        assert_eq!(
            reply["error"],
            "request line exceeds the 256 KiB frame budget"
        );
        assert!(out_rx.try_recv().is_err(), "nothing reached the hub");
        relay.close_task("task-budget").await;
        std::fs::remove_dir_all(dir).ok();
    }

    /// EF-2: a sabotaged proxy dir degrades the task to tool-less instead of
    /// failing it, and teardown never fails on a sabotaged socket path.
    #[tokio::test]
    async fn sabotaged_proxy_paths_never_fail_start_or_close() {
        let base = test_dir();
        let not_a_dir = base.join("not-a-dir");
        std::fs::write(&not_a_dir, b"x").expect("write file");
        let relay = ToolProxyRelay::with_overrides(not_a_dir, Duration::from_secs(5));
        let (out_tx, _rx) = fake_hub();
        assert!(relay.start_task("task-sab", out_tx.clone()).await.is_none());
        relay.close_task("task-sab").await;

        let (relay, dir) = test_relay();
        let handle = relay.start_task("task-sab2", out_tx).await.expect("task");
        std::fs::remove_file(&handle.socket_path).expect("remove socket");
        std::fs::create_dir(&handle.socket_path).expect("plant a directory");
        relay.close_task("task-sab2").await;
        assert_eq!(relay.active_task_count().await, 0);
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(base).ok();
    }

    /// LD-15 (host run): the proxy directory is traversable-but-not-listable
    /// 0711, and a symlinked directory is refused instead of followed — the
    /// relay runs as root, so following a link would aim its filesystem
    /// writes wherever task code points them.
    #[tokio::test]
    async fn proxy_dir_is_0711_and_symlinks_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let (relay, dir) = test_relay();
        let (hub_tx, _hub_rx) = fake_hub();
        let handle = relay.start_task("ld15", hub_tx).await;
        assert!(handle.is_some(), "task must start on a clean dir");
        let mode = std::fs::symlink_metadata(&dir)
            .expect("dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o711, "proxy dir must be traversable, not listable");
        relay.close_task("ld15").await;
        std::fs::remove_dir_all(&dir).ok();

        // A symlinked proxy dir is refused: no socket may be created through
        // the link, and the link target stays empty.
        let base = test_dir();
        let victim = base.join("victim");
        let link = base.join("link");
        std::fs::create_dir_all(&victim).expect("create victim");
        std::os::unix::fs::symlink(&victim, &link).expect("create link");
        let relay = ToolProxyRelay::with_overrides(link.clone(), Duration::from_secs(5));
        let (hub_tx, _hub_rx) = fake_hub();
        let handle = relay.start_task("ld15", hub_tx).await;
        assert!(handle.is_none(), "a symlinked proxy dir must be refused");
        assert!(
            std::fs::read_dir(&victim)
                .expect("read victim")
                .next()
                .is_none(),
            "no socket may be created through the symlink"
        );
        assert!(std::fs::symlink_metadata(&link)
            .expect("link still there")
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(&base).ok();
    }

    /// LD-15, root branch: a proxy directory owned by a foreign uid is
    /// refused instead of adopted. Meaningful only as root (an unprivileged
    /// process cannot prepare a foreign-owned directory).
    #[tokio::test]
    async fn proxy_dir_owned_by_a_foreign_uid_is_refused() {
        use std::os::unix::fs::MetadataExt;
        let own_uid = std::fs::metadata("/proc/self")
            .expect("proc metadata")
            .uid();
        if own_uid != 0 {
            return;
        }
        let (relay, dir) = test_relay();
        std::fs::create_dir_all(&dir).expect("create dir");
        std::os::unix::fs::chown(&dir, Some(65534), Some(65534)).expect("foreign owner");
        let (hub_tx, _hub_rx) = fake_hub();
        let handle = relay.start_task("ld15-foreign", hub_tx).await;
        assert!(
            handle.is_none(),
            "a foreign-owned proxy dir must be refused"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// LD-15, self-exec: with the task identity pinned, the per-task socket
    /// is handed to that uid (mode 0600) right after bind, before the task
    /// spawns; with no identity pinned (host dev runs) the socket is left
    /// untouched. Unprivileged branch pins the OWN uid (lchown to self is
    /// permitted); the root branch pins a stand-in uid.
    #[tokio::test]
    async fn socket_is_handed_to_the_task_identity() {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        const CHILD_MARKER: &str = "PEBBLE_WI400_LD15_CHILD";
        const TEST_PATH: &str = "tool_proxy::tests::socket_is_handed_to_the_task_identity";

        if std::env::var_os(CHILD_MARKER).is_none() {
            let own_uid = std::fs::metadata("/proc/self")
                .expect("proc metadata")
                .uid();
            let (uid, gid) = if own_uid == 0 {
                (65534, 65534)
            } else {
                (own_uid, own_uid)
            };
            let exe = std::env::current_exe().expect("test binary path");
            let out = std::process::Command::new(exe)
                .args(["--exact", TEST_PATH, "--nocapture"])
                .env(CHILD_MARKER, "1")
                .env("PEBBLE_TASK_UID", uid.to_string())
                .env("PEBBLE_TASK_GID", gid.to_string())
                .output()
                .expect("re-exec test binary");
            let text = String::from_utf8_lossy(&out.stdout);
            let stderr_text = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "child phase failed:\nstdout: {text}\nstderr: {stderr_text}"
            );
            assert!(
                text.contains("test result: ok. 1 passed"),
                "child phase ran no tests (drifted TEST_PATH?):\nstdout: {text}\nstderr: {stderr_text}"
            );
            return;
        }

        let task_uid: u32 = std::env::var("PEBBLE_TASK_UID")
            .expect("task uid")
            .parse()
            .expect("numeric");
        let (relay, dir) = test_relay();
        let (hub_tx, _hub_rx) = fake_hub();
        let handle = relay
            .start_task("ld15-handoff", hub_tx)
            .await
            .expect("task starts");
        let meta = std::fs::symlink_metadata(&handle.socket_path).expect("socket metadata");
        assert_eq!(meta.uid(), task_uid, "socket must be owned by the task uid");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "socket must be 0600"
        );
        relay.close_task("ld15-handoff").await;
        std::fs::remove_dir_all(&dir).ok();
    }
}
