//! Optional loopback-only Prometheus listener for LunarWing healthpoint
//! metrics (Phase 2 of `docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`).
//!
//! The textfile exporter (`health-metrics-exporter.sh`) is the primary
//! delivery mechanism; this listener exists only for hosts that cannot run a
//! textfile collector. It serves the `.prom` file the exporter writes on
//! `GET /metrics`, re-reading from disk on a refresh interval so a scraped
//! snapshot is never older than `HEALTH_METRICS_REFRESH_SECS`.
//!
//! Security posture:
//! - Binds **loopback only**; a non-loopback bind address is refused at
//!   startup rather than silently accepted.
//! - Optional bearer token (`HEALTH_METRICS_TOKEN`); when set, requests
//!   without a matching `Authorization: Bearer <token>` are rejected with
//!   401. The comparison is constant-time to avoid leaking the token.
//! - Only `/metrics` (and `/` redirect hint) respond; everything else is 404.
//! - No secrets in logs: the token is never logged, and error lines never
//!   include request bodies.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, header};
use hyper_util::rt::TokioIo;
use secrecy::{ExposeSecret, SecretString};
use tokio::net::TcpListener;

/// Served snapshot: last successfully read exposition body.
struct Snapshot {
    body: String,
    read_at: std::time::Instant,
}

/// Shared state: file to read, optional bearer token, cached snapshot.
struct State {
    prom_path: PathBuf,
    token: Option<SecretString>,
    snapshot: tokio::sync::RwLock<Snapshot>,
    refresh: Duration,
}

impl State {
    fn new(prom_path: PathBuf, token: Option<SecretString>, refresh: Duration) -> Self {
        Self {
            prom_path,
            token,
            snapshot: tokio::sync::RwLock::new(Snapshot {
                body: String::new(),
                read_at: std::time::Instant::now() - refresh,
            }),
            refresh,
        }
    }

    /// Re-read the `.prom` file if the cache is older than the refresh
    /// interval. Missing file is tolerated (exporter not run yet): the
    /// previous body keeps being served and `/metrics` reports 503 while the
    /// snapshot is empty.
    async fn refresh_if_stale(&self) {
        let needs = self.snapshot.read().await.read_at.elapsed() >= self.refresh;
        if !needs {
            return;
        }
        match tokio::fs::read_to_string(&self.prom_path).await {
            Ok(body) => {
                let mut snap = self.snapshot.write().await;
                snap.body = body;
                snap.read_at = std::time::Instant::now();
            }
            Err(err) => {
                tracing_warn(&format!(
                    "metrics file read failed ({}): serving previous snapshot",
                    err
                ));
            }
        }
    }

    /// Constant-time bearer check — see `AUTH_INVARIANT` in main.rs docs.
    fn authorized(&self, header_value: Option<&header::HeaderValue>) -> bool {
        let Some(expected) = self.token.as_ref() else {
            return true; // no token configured: loopback trust
        };
        let Some(value) = header_value.and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some(presented) = value.strip_prefix("Bearer ") else {
            return false;
        };
        constant_time_eq(expected.expose_secret().as_bytes(), presented.as_bytes())
    }
}

/// Constant-time byte comparison so a scrape attacker cannot bisect the token
/// through response timing. Length differences still short-circuit (leaking
/// only the token length, which is not secret material worth hiding).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn tracing_warn(msg: &str) {
    // This crate is intentionally dependency-light and has no tracing
    // subscriber of its own; stderr is what the service units capture.
    eprintln!("[lunarwing-health-metrics] {msg}");
}

type Resp = Response<Full<Bytes>>;

fn text_response(status: StatusCode, body: &str) -> Resp {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
        .unwrap_or_else(|_| unreachable!("static response parts are always valid"))
}

/// Recompute `lunarwing_health_report_age_seconds` at serve time from the
/// report timestamp series in the same exposition (WI#180 F5). The exporter
/// writes the age once at export time; if scheduling stops, the unchanged
/// textfile must still serve an age that advances with every scrape, or the
/// bundled stale-report rule can never fire. When the timestamp series is
/// absent or unparsable the body is returned unchanged.
fn rewrite_report_ages(body: &str, now_secs: u64) -> String {
    let mut ts: Option<u64> = None;
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("lunarwing_health_report_timestamp_seconds ") else {
            continue;
        };
        let value = rest.split_whitespace().next().unwrap_or("");
        if let Ok(v) = value.parse::<u64>() {
            ts = Some(v);
        }
        break;
    }
    let Some(ts) = ts else {
        return body.to_string();
    };
    let age = now_secs.saturating_sub(ts);
    let mut out = String::with_capacity(body.len() + 16);
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("lunarwing_health_report_age_seconds") {
            // rest is "", " <value>", or "{labels} <value>". Preserve any
            // label set and replace only the value. A label set attaches to
            // the metric name directly (no space) per exposition format.
            let (labels, _) = rest.split_once(' ').unwrap_or((rest, ""));
            out.push_str("lunarwing_health_report_age_seconds");
            if !labels.is_empty() {
                out.push_str(labels);
            }
            out.push(' ');
            out.push_str(&age.to_string());
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

async fn handle<B>(
    req: Request<B>,
    state: Arc<State>,
) -> std::result::Result<Resp, std::convert::Infallible>
where
    B: Send,
{
    // Token check first, route second: never reveal route existence to an
    // unauthenticated caller.
    let resp = if !state.authorized(req.headers().get(AUTHORIZATION)) {
        text_response(StatusCode::UNAUTHORIZED, "unauthorized\n")
    } else {
        match (req.method().as_str(), req.uri().path()) {
            ("GET", "/metrics") => {
                state.refresh_if_stale().await;
                let snap = state.snapshot.read().await;
                if snap.body.is_empty() {
                    text_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "metrics not yet available (exporter has not produced a report)\n",
                    )
                } else {
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    text_response(StatusCode::OK, &rewrite_report_ages(&snap.body, now_secs))
                }
            }
            ("GET", "/") => text_response(
                StatusCode::OK,
                "lunarwing healthpoint metrics: GET /metrics\n",
            ),
            ("GET", _) => text_response(StatusCode::NOT_FOUND, "not found\n"),
            _ => text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n"),
        }
    };
    Ok(resp)
}

/// Parse `HEALTH_METRICS_LISTEN` ("127.0.0.1:50000") and enforce loopback.
fn parse_listen(spec: &str) -> Result<SocketAddr> {
    let addr: SocketAddr = spec
        .parse()
        .with_context(|| format!("invalid HEALTH_METRICS_LISTEN value: {spec:?}"))?;
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_loopback() => Ok(addr),
        IpAddr::V6(ip) if ip.is_loopback() => Ok(addr),
        other => bail!(
            "refusing non-loopback bind address {other}; the metrics listener is loopback-only"
        ),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let listen_spec =
        std::env::var("HEALTH_METRICS_LISTEN").unwrap_or_else(|_| "127.0.0.1:50000".to_string());
    let token = std::env::var("HEALTH_METRICS_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .map(SecretString::from);
    let refresh_secs: u64 = std::env::var("HEALTH_METRICS_REFRESH_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(15);
    let base_dir = std::env::var("LUNARWING_BASE_DIR")
        .or_else(|_| std::env::var("IRONCLAW_BASE_DIR"))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.lunarwing")
        });
    let prom_path = std::env::var("HEALTH_METRICS_OUTPUT").map_or_else(
        |_| PathBuf::from(&base_dir).join("workspace/reports/health/metrics.prom"),
        PathBuf::from,
    );

    let addr = parse_listen(&listen_spec)?;
    let state = Arc::new(State::new(
        prom_path,
        token,
        Duration::from_secs(refresh_secs),
    ));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    eprintln!(
        "[lunarwing-health-metrics] serving /metrics on {addr} (refresh {refresh_secs}s, auth {})",
        if state.token.is_some() {
            "bearer"
        } else {
            "none"
        }
    );

    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing_warn(&format!("accept failed: {err}"));
                continue;
            }
        };
        // Loopback peer enforcement: a listener on a loopback address can
        // still be reached by non-loopback peers if the address family allows
        // it (e.g. ::1 mapped). Refuse anything not from loopback.
        let peer = stream.peer_addr().map(|a| a.ip()).ok();
        if let Some(peer_ip) = peer {
            let loopback = match peer_ip {
                IpAddr::V4(ip) => ip.is_loopback(),
                IpAddr::V6(ip) => ip.is_loopback(),
            };
            if !loopback {
                tracing_warn(&format!("dropping non-loopback peer {peer_ip}"));
                continue;
            }
        }
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| handle(req, Arc::clone(&state)));
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                tracing_warn(&format!("connection error: {err}"));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_must_be_loopback() {
        assert!(parse_listen("127.0.0.1:50000").is_ok());
        assert!(parse_listen("[::1]:50000").is_ok());
        assert!(parse_listen("0.0.0.0:9090").is_err());
        assert!(parse_listen("192.168.1.5:9090").is_err());
        assert!(parse_listen("not-an-address").is_err());
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    fn auth_header(v: &str) -> header::HeaderValue {
        v.parse().expect("test header values are valid")
    }

    #[tokio::test]
    async fn authorization_gating() {
        let state = State::new(
            PathBuf::from("/nonexistent/metrics.prom"),
            Some(SecretString::from("sekrit".to_string())),
            Duration::from_secs(60),
        );
        assert!(!state.authorized(None));
        assert!(!state.authorized(Some(&auth_header("Bearer wrong"))));
        assert!(state.authorized(Some(&auth_header("Bearer sekrit"))));

        let open = State::new(
            PathBuf::from("/nonexistent/metrics.prom"),
            None,
            Duration::from_secs(60),
        );
        assert!(open.authorized(None));
    }

    #[tokio::test]
    async fn routes_and_missing_metrics() {
        let state = Arc::new(State::new(
            PathBuf::from("/nonexistent/metrics.prom"),
            None,
            Duration::from_secs(60),
        ));
        let get = |path: &str, st: Arc<State>| {
            let req = Request::builder()
                .method("GET")
                .uri(path)
                .body(http_body_util::Empty::<Bytes>::new())
                .expect("test request is valid");
            handle(req, st)
        };
        let resp = get("/metrics", Arc::clone(&state))
            .await
            .expect("infallible");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let resp = get("/nope", Arc::clone(&state)).await.expect("infallible");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = get("/", Arc::clone(&state)).await.expect("infallible");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn report_age_rewrite_advances_with_serve_time() {
        // The exporter writes age 0 at export time; an hour later the same
        // unchanged file must serve an advancing age (WI#180 F5) or the
        // stale-report rule can never fire.
        let body = concat!(
            "lunarwing_health_report_timestamp_seconds 1750000000\n",
            "lunarwing_health_report_age_seconds 0\n",
            "lunarwing_health_overall_status 0\n",
        );
        let rewritten = rewrite_report_ages(body, 1_750_003_600);
        assert!(
            rewritten.contains("lunarwing_health_report_age_seconds 3600"),
            "age must advance with serve time: {rewritten}"
        );
        assert!(rewritten.contains("lunarwing_health_report_timestamp_seconds 1750000000"));
        assert!(rewritten.contains("lunarwing_health_overall_status 0"));
    }

    #[test]
    fn report_age_rewrite_preserves_labels_and_missing_series() {
        let with_labels = concat!(
            "lunarwing_health_report_timestamp_seconds 1750000000\n",
            "lunarwing_health_report_age_seconds{instance=\"a\"} 0\n",
        );
        let rewritten = rewrite_report_ages(with_labels, 1_750_000_001);
        assert!(
            rewritten.contains("lunarwing_health_report_age_seconds{instance=\"a\"} 1"),
            "labels preserved: {rewritten}"
        );
        // No timestamp series -> body returned unchanged (no invented age).
        let no_ts = "lunarwing_health_overall_status 0\n";
        assert_eq!(rewrite_report_ages(no_ts, 1_750_000_001), no_ts);
        // Timestamp newer than now -> age clamps to 0, never negative.
        let future = "lunarwing_health_report_timestamp_seconds 1750000100\nlunarwing_health_report_age_seconds 0\n";
        assert!(
            rewrite_report_ages(future, 1_750_000_001).contains("lunarwing_health_report_age_seconds 0"),
        );
    }
}
