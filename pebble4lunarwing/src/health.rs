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
    serve_listener(listener, state).await
}

async fn serve_listener(
    listener: TcpListener,
    state: Arc<WorkerState>,
) -> Result<(), Box<dyn std::error::Error>> {
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
    use http::StatusCode;

    if req.method() != Method::GET {
        return Ok(response(
            StatusCode::METHOD_NOT_ALLOWED,
            Bytes::from_static(b"Method Not Allowed"),
        ));
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
            Ok(json_response(
                if healthy {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                &body,
            ))
        }
        "/ready" => {
            let ready = state.is_ready();
            let connections = state.connections.load(Ordering::Relaxed);
            let body = serde_json::json!({
                "ready": ready,
                "connections": connections,
            });
            let status = if ready {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            Ok(json_response(status, &body))
        }
        _ => Ok(response(
            StatusCode::NOT_FOUND,
            Bytes::from_static(b"Not Found"),
        )),
    }
}

fn response(status: http::StatusCode, body: Bytes) -> Response<http_body_util::Full<Bytes>> {
    let mut response = Response::new(http_body_util::Full::new(body));
    *response.status_mut() = status;
    response
}

fn json_response(
    status: http::StatusCode,
    body: &serde_json::Value,
) -> Response<http_body_util::Full<Bytes>> {
    let mut response = response(status, Bytes::from(body.to_string()));
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_health_refuses_a_missing_runner_heartbeat() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind health");
        let addr = listener.local_addr().expect("health address");
        let state = Arc::new(WorkerState {
            worker_id: "http-probe".to_string(),
            ready: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
            start_time: Instant::now(),
            runner_state_file: Some(
                std::env::temp_dir().join(format!("pbr-missing-http-{}", std::process::id())),
            ),
        });
        let server = tokio::spawn(async move { serve_listener(listener, state).await.is_ok() });
        for path in ["/health", "/ready"] {
            let mut stream = tokio::net::TcpStream::connect(addr)
                .await
                .expect("connect health");
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .expect("send probe");
            let mut reply = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stream.read_to_string(&mut reply),
            )
            .await
            .expect("probe deadline")
            .expect("probe response");
            assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
        }
        server.abort();
    }

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
