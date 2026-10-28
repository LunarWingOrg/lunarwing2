use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use http::Response;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::protocol::WORKER_VERSION;

pub struct WorkerState {
    worker_id: String,
    ready: AtomicBool,
    connections: AtomicUsize,
    start_time: Instant,
    runner_state_file: Option<PathBuf>,
}

impl WorkerState {
    pub fn new(worker_id: &str) -> Self {
        Self {
            worker_id: worker_id.to_string(),
            ready: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            start_time: Instant::now(),
            runner_state_file: std::env::var_os("LUNARWING_TASK_RUNNER_STATE_FILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Relaxed);
    }

    pub fn increment_connections(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement_connections(&self) {
        self.connections.fetch_sub(1, Ordering::Relaxed);
    }

    fn runner_is_fresh(&self) -> bool {
        let Some(path) = &self.runner_state_file else {
            return true;
        };
        let Ok(body) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            return false;
        };
        let Some(stamp) = value["heartbeat_at"].as_f64() else {
            return false;
        };
        let Ok(now) = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
        else {
            return false;
        };
        let age = now - stamp;
        value["ready"] == true && (-5.0..=30.0).contains(&age)
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed) && self.runner_is_fresh()
    }
}

pub async fn serve(
    addr: SocketAddr,
    state: Arc<WorkerState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(addr).await?;
    eprintln!("[health] listening on http://{addr}");

    loop {
        let (stream, _) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| {
                let state = Arc::clone(&state);
                async move { handle_request(req, &state) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

#[allow(clippy::unnecessary_wraps, clippy::needless_pass_by_value)]
fn handle_request(
    req: Request<hyper::body::Incoming>,
    state: &WorkerState,
) -> Result<Response<http_body_util::Full<Bytes>>, Infallible> {
    if req.method() != Method::GET {
        return Ok(Response::builder()
            .status(405)
            .body(http_body_util::Full::new(Bytes::from_static(
                b"Method Not Allowed",
            )))
            .expect("response"));
    }

    match req.uri().path() {
        "/health" => {
            let healthy = state.runner_state_file.is_none() || state.is_ready();
            let body = serde_json::json!({
                "status": if healthy { "ok" } else { "not_ready" },
                "uptime_seconds": state.start_time.elapsed().as_secs(),
                "worker_id": state.worker_id,
                "version": WORKER_VERSION,
                "mode": "websocket",
            });
            Ok(json_response(if healthy { 200 } else { 503 }, &body))
        }
        "/ready" => {
            let ready = state.is_ready();
            let connections = state.connections.load(Ordering::Relaxed);
            let body = serde_json::json!({
                "ready": ready,
                "connections": connections,
            });
            let status = if ready { 200 } else { 503 };
            Ok(json_response(status, &body))
        }
        _ => Ok(Response::builder()
            .status(404)
            .body(http_body_util::Full::new(Bytes::from_static(b"Not Found")))
            .expect("response")),
    }
}

fn json_response(status: u16, body: &serde_json::Value) -> Response<http_body_util::Full<Bytes>> {
    let json = serde_json::to_string(body).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(Bytes::from(json)))
        .expect("response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_heartbeat_is_required_when_configured() {
        let path = std::env::temp_dir().join(format!("pbr-health-{}.json", std::process::id()));
        let state = WorkerState {
            worker_id: "test".to_string(),
            ready: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
            start_time: Instant::now(),
            runner_state_file: Some(path.clone()),
        };
        assert!(!state.is_ready());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs_f64();
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "ready": true,
                "heartbeat_at": now,
            }))
            .expect("state json"),
        )
        .expect("write state");
        assert!(state.is_ready());
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "ready": true,
                "heartbeat_at": now - 31.0,
            }))
            .expect("state json"),
        )
        .expect("write stale state");
        assert!(!state.is_ready());
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "ready": true,
                "heartbeat_at": now + 6.0,
            }))
            .expect("state json"),
        )
        .expect("write future state");
        assert!(!state.is_ready());
        std::fs::remove_file(path).ok();
    }
}
