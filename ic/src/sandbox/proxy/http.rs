//! HTTP proxy server for sandboxed network access.
//!
//! This proxy runs on the host and handles all network requests from containers.
//! It validates requests against the allowlist and injects credentials when needed.
//!
//! ```text
//! Container ──► http_proxy=host.docker.internal:PORT ──► This Proxy ──► Internet
//!                                                             │
//!                                                             ├─► Validate domain
//!                                                             ├─► Inject credentials
//!                                                             └─► Log requests
//! ```

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

use crate::sandbox::error::{Result, SandboxError};
use crate::sandbox::proxy::policy::{NetworkDecision, NetworkPolicyDecider, NetworkRequest};
use crate::secrets::CredentialLocation;

/// State shared across proxy connections.
struct ProxyState {
    /// Policy decider for network requests.
    decider: Arc<dyn NetworkPolicyDecider>,
    /// Credential resolver (maps secret names to values).
    credential_resolver: Arc<dyn CredentialResolver>,
    /// Reject non-public destinations and pin DNS for proxy-enforced
    /// containers (WI#155). Advisory-mode clients are unaffected.
    public_only: bool,
    /// Request counter for logging.
    request_count: std::sync::atomic::AtomicU64,
    /// Whether the proxy is running.
    running: std::sync::atomic::AtomicBool,
}

/// Resolves secret names to their values.
#[async_trait::async_trait]
pub trait CredentialResolver: Send + Sync {
    /// Get the value of a secret by name.
    async fn resolve(&self, name: &str) -> Option<String>;
}

/// A credential resolver that uses environment variables.
pub struct EnvCredentialResolver;

#[async_trait::async_trait]
impl CredentialResolver for EnvCredentialResolver {
    async fn resolve(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// A credential resolver that returns nothing (for testing).
pub struct NoCredentialResolver;

#[async_trait::async_trait]
impl CredentialResolver for NoCredentialResolver {
    async fn resolve(&self, _name: &str) -> Option<String> {
        None
    }
}

/// HTTP proxy server.
pub struct HttpProxy {
    state: Arc<ProxyState>,
    addr: RwLock<Option<SocketAddr>>,
    shutdown_tx: RwLock<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl HttpProxy {
    /// Create a new HTTP proxy (advisory egress: no destination filtering
    /// beyond the allowlist decider).
    pub fn new(
        decider: Arc<dyn NetworkPolicyDecider>,
        credential_resolver: Arc<dyn CredentialResolver>,
    ) -> Self {
        Self::with_egress_policy(decider, credential_resolver, false)
    }

    /// Create a HTTP proxy; `public_only` additionally rejects non-public
    /// destinations and pins DNS resolution for every outbound connection.
    ///
    /// The effect is not shared HTTP-client state: per-request clients are
    /// built with explicit DNS pinning and disabled redirects so environment
    /// proxies cannot bypass the pin and redirects re-enter policy checks.
    pub(super) fn with_egress_policy(
        decider: Arc<dyn NetworkPolicyDecider>,
        credential_resolver: Arc<dyn CredentialResolver>,
        public_only: bool,
    ) -> Self {
        Self {
            state: Arc::new(ProxyState {
                decider,
                credential_resolver,
                public_only,
                request_count: std::sync::atomic::AtomicU64::new(0),
                running: std::sync::atomic::AtomicBool::new(false),
            }),
            addr: RwLock::new(None),
            shutdown_tx: RwLock::new(None),
        }
    }

    /// Start the proxy server on the given port (0 for auto-assign).
    pub async fn start(&self, port: u16) -> Result<SocketAddr> {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
            .await
            .map_err(|e| SandboxError::ProxyError {
                reason: format!("failed to bind: {}", e),
            })?;

        let addr = listener
            .local_addr()
            .map_err(|e| SandboxError::ProxyError {
                reason: format!("failed to get local addr: {}", e),
            })?;

        *self.addr.write().await = Some(addr);

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        self.state
            .running
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let state = self.state.clone();

        tokio::spawn(async move {
            tracing::info!("Sandbox proxy started on {}", addr);

            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((stream, _)) => {
                                let io = TokioIo::new(stream);
                                let state = state.clone();

                                tokio::spawn(async move {
                                    let service = service_fn(move |req| {
                                        let state = state.clone();
                                        async move { handle_request(req, state).await }
                                    });

                                    if let Err(e) = http1::Builder::new()
                                        .preserve_header_case(true)
                                        .title_case_headers(true)
                                        .serve_connection(io, service)
                                        .with_upgrades()
                                        .await
                                    {
                                        tracing::debug!("Proxy connection error: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::error!("Proxy accept error: {}", e);
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        tracing::debug!("Sandbox proxy shutting down");
                        break;
                    }
                }
            }

            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        });

        Ok(addr)
    }

    /// Stop the proxy server.
    pub async fn stop(&self) {
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }
    }

    /// Get the address the proxy is listening on.
    pub async fn addr(&self) -> Option<SocketAddr> {
        *self.addr.read().await
    }

    /// Check if the proxy is running.
    pub fn is_running(&self) -> bool {
        self.state.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the number of requests handled.
    pub fn request_count(&self) -> u64 {
        self.state
            .request_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Handle an incoming proxy request.
async fn handle_request(
    req: Request<hyper::body::Incoming>,
    state: Arc<ProxyState>,
) -> std::result::Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    state
        .request_count
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    // Handle CONNECT method for HTTPS tunneling
    if req.method() == Method::CONNECT {
        return Ok(handle_connect(req, state).await);
    }

    // For HTTP requests, validate and forward
    let uri = req.uri().to_string();
    let method = req.method().to_string();

    let network_req = match NetworkRequest::from_url(&method, &uri) {
        Some(r) => r,
        None => {
            tracing::warn!("Proxy: invalid URL: {}", uri);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "Invalid URL".to_string(),
            ));
        }
    };

    // Make policy decision
    let decision = state.decider.decide(&network_req).await;

    match decision {
        NetworkDecision::Deny { reason } => {
            tracing::info!("Proxy: blocked {} {} - {}", method, uri, reason);
            Ok(error_response(StatusCode::FORBIDDEN, deny_body(reason)))
        }
        NetworkDecision::Allow | NetworkDecision::AllowWithCredentials { .. } => {
            // Forward the request
            forward_request(req, decision, state).await
        }
    }
}

/// Handle CONNECT method for HTTPS tunneling.
///
/// Establishes a bidirectional TCP tunnel between the client and the target host.
/// Returns 200 OK to signal the client to begin TLS over the upgraded connection.
///
/// NOTE: Credential injection is not possible through CONNECT tunnels since the proxy
/// cannot inspect or modify TLS-encrypted traffic without MITM. Containers that need
/// authenticated HTTPS should fetch credentials via the orchestrator's
/// `GET /worker/{id}/credentials` endpoint and set them as environment variables.
async fn handle_connect(
    req: Request<hyper::body::Incoming>,
    state: Arc<ProxyState>,
) -> Response<BoxBody<Bytes, Infallible>> {
    // Extract host:port from CONNECT target (e.g. "api.github.com:443")
    let authority = match req.uri().authority() {
        Some(a) => a.clone(),
        None => {
            return error_response(StatusCode::BAD_REQUEST, "Missing host".to_string());
        }
    };

    let host = authority.host().to_string();
    let target_addr = authority.as_str().to_string();

    // Check if host is allowed
    let network_req = NetworkRequest {
        method: "CONNECT".to_string(),
        url: format!("https://{}", host),
        host: host.clone(),
        path: "/".to_string(),
    };

    let decision = state.decider.decide(&network_req).await;

    if let NetworkDecision::Deny { reason } = decision {
        tracing::info!("Proxy: blocked CONNECT {} - {}", host, reason);
        return error_response(StatusCode::FORBIDDEN, deny_body(reason));
    }

    // Enforced egress: reject non-public targets and pin the CONNECT to the
    // resolved public addresses (no re-resolution, no env-proxy bypass).
    let pinned = if state.public_only {
        match super::egress::resolve_public(&host, authority.port_u16().unwrap_or(443)).await {
            Ok(addresses) => Some(addresses),
            Err(reason) => return error_response(StatusCode::FORBIDDEN, deny_body(reason)),
        }
    } else {
        None
    };

    tracing::debug!("Proxy: allowing CONNECT to {}", target_addr);

    // Spawn a fire-and-forget task to establish the tunnel after the upgrade
    // completes.  The 30-minute timeout guarantees every tunnel task terminates
    // even if the remote peer hangs, so no `JoinSet` tracking is needed.
    // On process exit these tasks are dropped by the runtime.
    let target = target_addr.clone();
    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                let mut client_stream = TokioIo::new(upgraded);
                let connect = async {
                    if let Some(addresses) = pinned {
                        TcpStream::connect(addresses.as_slice()).await
                    } else {
                        TcpStream::connect(&target).await
                    }
                };
                match tokio::time::timeout(std::time::Duration::from_secs(10), connect).await {
                    Ok(Ok(mut server_stream)) => {
                        let tunnel_timeout = std::time::Duration::from_secs(30 * 60);
                        match tokio::time::timeout(
                            tunnel_timeout,
                            tokio::io::copy_bidirectional(&mut client_stream, &mut server_stream),
                        )
                        .await
                        {
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => {
                                tracing::debug!("Proxy: tunnel to {} closed: {}", target, e);
                            }
                            Err(_) => {
                                tracing::info!(
                                    "Proxy: tunnel to {} timed out after 30m, closing",
                                    target
                                );
                            }
                        }
                    }
                    error => {
                        tracing::debug!(?error, "Proxy: tunnel connection failed");
                    }
                }
            }
            Err(e) => {
                tracing::error!("Proxy: upgrade failed for {}: {}", target, e);
            }
        }
    });

    // Return 200 OK so the client begins the TLS handshake over the upgraded connection
    make_response(StatusCode::OK, empty_body())
}

/// Forward a request to the target server.
async fn forward_request(
    req: Request<hyper::body::Incoming>,
    decision: NetworkDecision,
    state: Arc<ProxyState>,
) -> std::result::Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    let uri = req.uri().clone();

    // Spellings of the injected credential that an upstream may echo back in
    // a response header (WI#395: an always-HTTPS edge answers plain-HTTP
    // requests with `301 Location: https://host/path?name=<secret>`).
    let mut echo_spellings: Vec<String> = Vec::new();

    // Enforced egress: per-request client with DNS pinning, no redirect
    // following (redirects re-enter allowlist checks instead of being followed
    // on the host), and no environment proxy settings that could bypass DNS
    // pinning.
    let client = match forwarding_client(&uri, state.public_only).await {
        Ok(client) => client,
        Err(reason) => return Ok(error_response(StatusCode::FORBIDDEN, deny_body(reason))),
    };

    let method = req.method().clone();
    let method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut builder = client.request(method, uri.to_string());

    // Copy headers (except hop-by-hop headers). Accept-Encoding is held
    // back: a credential-injected exchange must request an identity-encoded
    // body so the response can be byte-scrubbed (WI#395 run-111 HA-2);
    // ordinary requests forward the container's original value.
    let mut accept_encoding: Option<String> = None;
    for (name, value) in req.headers() {
        if !is_hop_by_hop_header(name.as_str())
            && let Ok(v) = value.to_str()
        {
            if name.as_str().eq_ignore_ascii_case("accept-encoding") {
                accept_encoding = Some(v.to_string());
                continue;
            }
            builder = builder.header(name.as_str(), v);
        }
    }

    // Inject credentials if needed
    if let NetworkDecision::AllowWithCredentials {
        secret_name,
        location,
    } = decision
    {
        if let Some(credential) = state.credential_resolver.resolve(&secret_name).await {
            // The raw spelling covers header/bearer echoes (they embed the
            // value verbatim); the encoded one covers query echoes.
            echo_spellings = credential_echo_spellings(&credential);
            builder = match location {
                CredentialLocation::AuthorizationBearer => {
                    builder.header("Authorization", format!("Bearer {}", credential))
                }
                CredentialLocation::Header { name, prefix } => {
                    let value = match prefix {
                        Some(p) => format!("{}{}", p, credential),
                        None => credential.clone(),
                    };
                    builder.header(name, value)
                }
                CredentialLocation::QueryParam { name } => builder.query(&[(name, credential)]),
                // Known limitation: AuthorizationBasic requires the proxy to
                // construct a Base64 username:password pair from a single secret,
                // and UrlPath requires rewriting the request URI. Neither is
                // implemented yet. Containers needing these auth styles should
                // fetch credentials via the orchestrator's GET /worker/{id}/credentials
                // endpoint and set them directly.
                CredentialLocation::AuthorizationBasic { .. }
                | CredentialLocation::UrlPath { .. } => {
                    tracing::warn!(
                        "Proxy: credential location {:?} not supported for forward proxy, skipping",
                        location
                    );
                    builder
                }
            };
            tracing::debug!("Proxy: injected credential for {}", secret_name);
        } else {
            tracing::warn!("Proxy: credential {} not found", secret_name);
        }
    }

    if echo_spellings.is_empty() {
        if let Some(v) = accept_encoding {
            builder = builder.header("accept-encoding", v);
        }
    } else {
        // WI#395 (run-111 HA-2): a compressed body cannot be byte-scrubbed
        // for echoed credentials; ask the upstream for identity encoding on
        // credential-injected exchanges (an opaque content or transfer
        // coding is refused below rather than forwarded blind).
        builder = builder.header("accept-encoding", "identity");
    }

    // Copy body
    let body_bytes = match req.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            tracing::error!("Proxy: failed to read request body: {}", e);
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to read body".to_string(),
            ));
        }
    };

    if !body_bytes.is_empty() {
        builder = builder.body(body_bytes.to_vec());
    }

    // Send the request
    match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let headers = response.headers().clone();

            match response.bytes().await {
                Ok(body) => {
                    let mut resp_builder = Response::builder().status(status.as_u16());
                    let mut body = body;

                    // WI#395 (run-111 HA-2): 3xx bodies echo the redirect
                    // target URL on common edges (Go, Apache, Express), so a
                    // credential-injected exchange scrubs the body with the
                    // same spellings as the headers. A body whose content or
                    // transfer coding is not inspectable is refused rather
                    // than forwarded with a possible echo blind.
                    let mut body_scrubbed = false;
                    if !echo_spellings.is_empty() {
                        if response_body_coding_is_opaque(&headers) {
                            tracing::warn!(
                                "Proxy: refused content-encoded response body that cannot be scrubbed for echoed credentials"
                            );
                            return Ok(error_response(
                                StatusCode::BAD_GATEWAY,
                                "Failed to read response".to_string(),
                            ));
                        }
                        if let Some(scrubbed) = scrub_credential_echoes(&body, &echo_spellings) {
                            tracing::warn!("Proxy: scrubbed echoed credential from response body");
                            body = Bytes::from(scrubbed);
                            body_scrubbed = true;
                        }
                    }

                    resp_builder = forward_scrubbed_headers(
                        resp_builder,
                        &headers,
                        &echo_spellings,
                        body_scrubbed,
                    );

                    Ok(make_response_from_builder(resp_builder, full_body(body)))
                }
                Err(e) => {
                    // Body-read errors can carry the request URL in their
                    // Display; strip it before logging (WI#395). The body
                    // returned to the container stays a fixed generic message.
                    tracing::error!("Proxy: failed to read response body: {}", e.without_url());
                    Ok(error_response(
                        StatusCode::BAD_GATEWAY,
                        "Failed to read response".to_string(),
                    ))
                }
            }
        }
        Err(e) => {
            // reqwest's Display appends the full request URL; with a QueryParam
            // injection that URL carries the secret, and both this log line and
            // the container-visible 502 body would hand it back to untrusted
            // code (WI#395). Strip the URL before either use.
            let e = e.without_url();
            tracing::error!("Proxy: request failed: {}", e);
            Ok(error_response(
                StatusCode::BAD_GATEWAY,
                format!("Request failed: {}", e),
            ))
        }
    }
}

/// Spellings of an injected credential that an upstream may echo back in a
/// response header (WI#395): the raw value (header/bearer echoes embed it
/// verbatim) plus the form-encoded spelling reqwest `.query()` puts on the
/// wire — an always-HTTPS edge echoes that form in a redirect `Location`.
fn credential_echo_spellings(credential: &str) -> Vec<String> {
    let encoded: String = url::form_urlencoded::byte_serialize(credential.as_bytes()).collect();
    let mut spellings = vec![credential.to_string()];
    if encoded != credential {
        spellings.push(encoded);
    }
    spellings
}

/// Replace every credential spelling in `value` with `[REDACTED]`.
///
/// Returns the scrubbed bytes only when something was replaced. Works on raw
/// bytes so non-UTF-8 header values cannot smuggle an echo through.
fn scrub_credential_echoes(value: &[u8], spellings: &[String]) -> Option<Vec<u8>> {
    const MARKER: &[u8] = b"[REDACTED]";
    let mut out = value.to_vec();
    let mut scrubbed = false;
    for spelling in spellings {
        let needle = spelling.as_bytes();
        let mut search_from = 0;
        while let Some(offset) = find_subslice(&out[search_from..], needle) {
            let at = search_from + offset;
            out.splice(at..at + needle.len(), MARKER.iter().copied());
            scrubbed = true;
            search_from = at + MARKER.len();
        }
    }
    scrubbed.then_some(out)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Minimum byte length a credential spelling must have before header
/// values are scrubbed for it (WI#395 run-111 HA-9): byte-splicing a 1-3
/// byte sequence corrupts unrelated header values (charset parameters,
/// dates, lengths) while such a value carries no real secret.
const MIN_HEADER_SCRUB_LEN: usize = 4;

/// Outcome of scrubbing one forwarded header value (WI#395).
enum HeaderScrub {
    /// Nothing matched; forward the value unchanged.
    Unchanged,
    /// An exact spelling was spliced out; surrounding text is preserved.
    Spliced(Vec<u8>),
    /// A re-encoded echo was detected (run-111 HA-5); the whole value is
    /// replaced because the exact bytes to remove cannot be identified.
    Replaced,
}

/// Scrub echoed credential spellings from one header value.
///
/// Exact spellings (raw + form-encoded) are spliced out. Re-encoded echoes
/// — lowercase hex, double encoding, `+` for space (run-111 HA-5) — are
/// detected by percent-decoding up to two rounds and replace the whole
/// value. `spellings[0]` is the raw spelling (see
/// [`credential_echo_spellings`]).
fn scrub_header_value(value: &[u8], spellings: &[String]) -> HeaderScrub {
    let Some(raw) = spellings.first() else {
        return HeaderScrub::Unchanged;
    };
    if raw.len() < MIN_HEADER_SCRUB_LEN {
        return HeaderScrub::Unchanged;
    }
    if let Some(scrubbed) = scrub_credential_echoes(value, spellings) {
        return HeaderScrub::Spliced(scrubbed);
    }
    if value_contains_reencoded_secret(value, raw.as_bytes()) {
        return HeaderScrub::Replaced;
    }
    HeaderScrub::Unchanged
}

/// Copy upstream headers onto the response builder, scrubbing echoed
/// credential spellings from values (WI#395). `drop_content_length` forces
/// the upstream Content-Length to be dropped so hyper re-frames the
/// response from the actual (possibly scrubbed) body.
///
/// Logs name the header only — never the value.
fn forward_scrubbed_headers(
    mut builder: hyper::http::response::Builder,
    headers: &http::HeaderMap,
    spellings: &[String],
    drop_content_length: bool,
) -> hyper::http::response::Builder {
    for (name, value) in headers.iter() {
        if is_hop_by_hop_header(name.as_str()) {
            continue;
        }
        if name.as_str().eq_ignore_ascii_case("content-length") {
            // A spliced marker would corrupt the length; whenever the value
            // or the body was touched the header is dropped and hyper
            // re-frames from the body (run-111 HA-2/HA-9).
            if drop_content_length
                || !matches!(
                    scrub_header_value(value.as_bytes(), spellings),
                    HeaderScrub::Unchanged
                )
            {
                continue;
            }
            builder = builder.header(name.as_str(), value.as_bytes());
            continue;
        }
        builder = match scrub_header_value(value.as_bytes(), spellings) {
            HeaderScrub::Unchanged => builder.header(name.as_str(), value.as_bytes()),
            HeaderScrub::Spliced(scrubbed) => {
                tracing::warn!(
                    "Proxy: scrubbed echoed credential from response header {}",
                    name
                );
                match http::HeaderValue::from_bytes(&scrubbed) {
                    Ok(v) => builder.header(name.as_str(), v),
                    Err(_) => {
                        tracing::warn!(
                            "Proxy: dropped response header {} whose scrubbed value was not valid HTTP",
                            name
                        );
                        builder
                    }
                }
            }
            HeaderScrub::Replaced => {
                tracing::warn!(
                    "Proxy: replaced response header {} carrying a re-encoded credential echo",
                    name
                );
                builder.header(name.as_str(), "[REDACTED]")
            }
        };
    }
    builder
}

/// True when the upstream response body is encoded in a way the proxy
/// cannot byte-scrub (WI#395 run-111 HA-2, WI#419 KH-1, run-137 IV-1).
/// Missing, empty and `identity` content codings are inspectable, and so
/// is a single `chunked` transfer coding that hyper removes (the last
/// element of the last field). `chunked,` or a trailing empty field leave
/// chunk framing in the body; repeated `chunked` can leave an inner layer.
/// Any other transfer-coding token (`gzip`, `identity`, `chunked;x=1`) is
/// refused regardless of whether hyper removes the framing. Every
/// `Content-Encoding` / `Transfer-Encoding` field is checked — a duplicate
/// field cannot hide a coding behind an `identity` first field — with each
/// comma-separated token checked individually, and an unreadable value
/// counts as opaque rather than fail-open.
fn response_body_coding_is_opaque(headers: &http::HeaderMap) -> bool {
    fn field_is_opaque(value: &http::HeaderValue, allowed: &str) -> bool {
        match value.to_str() {
            Ok(v) => v.split(',').any(|token| {
                let token = token.trim();
                !token.is_empty() && !token.eq_ignore_ascii_case(allowed)
            }),
            Err(_) => true,
        }
    }
    if headers
        .get_all("content-encoding")
        .iter()
        .any(|v| field_is_opaque(v, "identity"))
    {
        return true;
    }
    let transfer = headers.get_all("transfer-encoding");
    let Some(last) = transfer.iter().next_back() else {
        return false;
    };
    if transfer.iter().any(|v| field_is_opaque(v, "chunked")) {
        return true;
    }
    // hyper removes exactly one chunked layer, and only when the last list
    // element of the last field is `chunked`; `chunked,` or a trailing empty
    // field leave framing intact, and repeated `chunked` can leave an inner
    // layer (run-137 IV-1).
    let hyper_dechunks = last
        .to_str()
        .ok()
        .and_then(|v| v.rsplit(',').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("chunked"));
    let chunked_tokens: usize = transfer
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|v| v.split(',').filter(|t| !t.trim().is_empty()).count())
        .sum();
    !(hyper_dechunks && chunked_tokens == 1)
}

/// Percent-decode `%XX` escapes for one round; `+` is optionally read as a
/// space (query-string convention). Undecodable escapes pass through
/// verbatim so partial sequences cannot corrupt the scan.
fn percent_decode_round(bytes: &[u8], plus_as_space: bool) -> Vec<u8> {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(if b == b'+' && plus_as_space { b' ' } else { b });
        i += 1;
    }
    out
}

/// Detect a re-encoded echo of `secret` in `value`: the raw secret appears
/// after up to two percent-decode rounds, either of which may read `+` as
/// a space (run-111 HA-5 — lowercase hex, double encoding and form-style
/// spaces all evade exact two-spelling matching; direct matches are
/// handled by [`scrub_credential_echoes`]).
fn value_contains_reencoded_secret(value: &[u8], secret: &[u8]) -> bool {
    if secret.is_empty() {
        return false;
    }
    for first_plus_as_space in [false, true] {
        let once = percent_decode_round(value, first_plus_as_space);
        if find_subslice(&once, secret).is_some() {
            return true;
        }
        for second_plus_as_space in [false, true] {
            let twice = percent_decode_round(&once, second_plus_as_space);
            if find_subslice(&twice, secret).is_some() {
                return true;
            }
        }
    }
    false
}

/// Build the forwarding HTTP client for one proxied request.
///
/// Both modes use per-request clients that never follow redirects on the
/// host (a 3xx is returned to the container, whose follow-up request
/// re-enters the allowlist check) and ignore environment proxy settings
/// (so they cannot bypass DNS pinning). Enforced mode additionally pins
/// every outbound connection to the previously resolved public addresses.
async fn forwarding_client(
    uri: &hyper::Uri,
    public_only: bool,
) -> std::result::Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30));
    if public_only {
        let url = url::Url::parse(&uri.to_string()).map_err(|_| "invalid proxy URL")?;
        let host = url.host_str().ok_or("missing proxy host")?;
        let port = url.port_or_known_default().ok_or("invalid proxy port")?;
        let addresses = super::egress::resolve_public(host, port).await?;
        builder = builder.resolve_to_addrs(host, &addresses);
    }
    builder
        .build()
        .map_err(|e| format!("cannot initialize proxy client: {e}"))
}

/// Check if a header is hop-by-hop (should not be forwarded).
fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Build a response with guaranteed success (valid status + simple body cannot fail).
fn make_response(
    status: StatusCode,
    body: BoxBody<Bytes, Infallible>,
) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(status)
        .body(body)
        .unwrap_or_else(|_| {
            let mut resp = Response::new(
                Full::new(Bytes::from("Internal error"))
                    .map_err(|_| unreachable!())
                    .boxed(),
            );
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp
        })
}

/// Finalize a partially-built response, falling back to 500 on builder error.
fn make_response_from_builder(
    builder: hyper::http::response::Builder,
    body: BoxBody<Bytes, Infallible>,
) -> Response<BoxBody<Bytes, Infallible>> {
    builder.body(body).unwrap_or_else(|_| {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(full_body(Bytes::from("Response build error")))
            .unwrap_or_else(|_| {
                Response::new(
                    Full::new(Bytes::from("Internal error"))
                        .map_err(|_| unreachable!())
                        .boxed(),
                )
            })
    })
}

/// Create an error response.
fn error_response(status: StatusCode, message: String) -> Response<BoxBody<Bytes, Infallible>> {
    make_response_from_builder(
        Response::builder()
            .status(status)
            .header("Content-Type", "text/plain"),
        full_body(Bytes::from(message)),
    )
}

/// Body for policy denials. The `sandbox-proxy denied:` prefix is a stable
/// contract so an agent reading command output inside a sandboxed container
/// can tell a proxy denial apart from an error served by the remote service
/// (pinned by test).
fn deny_body(reason: String) -> String {
    format!("sandbox-proxy denied: {reason}")
}

/// Create an empty body.
fn empty_body() -> BoxBody<Bytes, Infallible> {
    Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed()
}

/// Create a body from bytes.
fn full_body(bytes: Bytes) -> BoxBody<Bytes, Infallible> {
    Full::new(bytes).map_err(|_| unreachable!()).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::proxy::allowlist::DomainAllowlist;
    use crate::sandbox::proxy::policy::DefaultPolicyDecider;
    use crate::secrets::{CredentialLocation, CredentialMapping};
    use tracing_test::traced_test;

    /// Resolves every secret name to a fixed value (for testing).
    struct FixedCredentialResolver(String);

    #[async_trait::async_trait]
    impl CredentialResolver for FixedCredentialResolver {
        async fn resolve(&self, _name: &str) -> Option<String> {
            Some(self.0.clone())
        }
    }

    /// Bind an ephemeral loopback port and keep it bound, accepting and
    /// dropping every connection, so sends to it fail deterministically (no
    /// bind/drop/connect TOCTOU with another process grabbing the port).
    async fn closed_loopback_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((conn, _)) = listener.accept().await else {
                    return;
                };
                drop(conn);
            }
        });
        port
    }

    #[tokio::test]
    async fn test_proxy_starts_and_stops() {
        let allowlist = DomainAllowlist::new(&["example.com".to_string()]);
        let decider = Arc::new(DefaultPolicyDecider::new(allowlist, vec![]));
        let resolver = Arc::new(NoCredentialResolver);

        let proxy = HttpProxy::new(decider, resolver);

        let addr = proxy.start(0).await.unwrap();
        assert!(proxy.is_running());
        assert!(addr.port() > 0);

        proxy.stop().await;
        // Give it a moment to shut down
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[test]
    fn test_hop_by_hop_headers() {
        assert!(is_hop_by_hop_header("connection"));
        assert!(is_hop_by_hop_header("Connection"));
        assert!(is_hop_by_hop_header("transfer-encoding"));
        assert!(!is_hop_by_hop_header("content-type"));
        assert!(!is_hop_by_hop_header("authorization"));
    }

    #[test]
    fn test_make_response_does_not_panic() {
        let resp = make_response(StatusCode::OK, empty_body());
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = error_response(StatusCode::FORBIDDEN, "denied".to_string());
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// The deny prefix is a contract consumed by sandboxed agents reading
    /// command output: it must always identify the proxy as the denier.
    #[test]
    fn test_deny_body_carries_proxy_prefix() {
        assert_eq!(
            deny_body("domain not in allowlist: evil.example".to_string()),
            "sandbox-proxy denied: domain not in allowlist: evil.example"
        );
    }

    /// WI#395 (run-111 HA-5/HA-9): exact two-spelling matching misses
    /// re-encoded echoes (lowercase hex, double encoding); those replace the
    /// whole value. Short spellings are never spliced — a 1-3 byte "secret"
    /// would corrupt unrelated header values.
    #[test]
    fn test_scrub_header_value_catches_reencoded_echoes() {
        let secret = "qp+/=re-1a2b";
        let spellings = credential_echo_spellings(secret);

        // Lowercase-hex percent-encoding of the raw secret (.NET style).
        let lower: String = secret.bytes().map(|b| format!("%{b:02x}")).collect();
        let value = format!("https://edge.example/redir?token={lower}");
        assert!(
            matches!(
                scrub_header_value(value.as_bytes(), &spellings),
                HeaderScrub::Replaced
            ),
            "lowercase-hex echo must be detected: {value}"
        );

        // Double encoding: the form-encoded spelling, percent-encoded again.
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        let double: String = encoded
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        assert!(
            matches!(
                scrub_header_value(double.as_bytes(), &spellings),
                HeaderScrub::Replaced
            ),
            "double-encoded echo must be detected: {double}"
        );

        // A verbatim echo still splices; unrelated values pass through.
        assert!(matches!(
            scrub_header_value(format!("Bearer {secret}").as_bytes(), &spellings),
            HeaderScrub::Spliced(_)
        ));
        assert!(matches!(
            scrub_header_value(b"charset=utf-8", &spellings),
            HeaderScrub::Unchanged
        ));

        // HA-9 (run-114 HJ-4): a 1-3 byte spelling never rewrites unrelated
        // values — "utf" DOES occur in "charset=utf-8", so dropping the
        // guard would splice it and fail this pin (the previous "abc"
        // fixture never occurred in the value and pinned nothing).
        let short = credential_echo_spellings("utf");
        assert!(matches!(
            scrub_header_value(b"charset=utf-8", &short),
            HeaderScrub::Unchanged
        ));
        assert!(matches!(
            scrub_header_value(b"charset=utf-8; q=0.9", &short),
            HeaderScrub::Unchanged
        ));

        // HQ-3: four raw bytes must still scrub, including the encoded
        // spelling. Raising the floor to five would silently skip this echo.
        let at_floor = credential_echo_spellings("ab+c");
        assert!(matches!(
            scrub_header_value(b"x=ab%2Bc", &at_floor),
            HeaderScrub::Spliced(value) if value == b"x=[REDACTED]"
        ));
    }

    #[tokio::test]
    async fn enforced_proxy_blocks_allowlisted_loopback_for_http_and_connect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let proxy = HttpProxy::with_egress_policy(
            Arc::new(crate::sandbox::proxy::AllowAllDecider),
            Arc::new(NoCredentialResolver),
            true,
        );
        let addr = proxy.start(0).await.unwrap();
        for request in [
            "GET http://127.0.0.1:43210/ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            "CONNECT 127.0.0.1:43210 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            "GET http://[::ffff:127.0.0.1]:43210/ HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        ] {
            let mut client = TcpStream::connect(addr).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                client.read_to_string(&mut response),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(response.starts_with("HTTP/1.1 403"), "{response}");
            assert!(response.contains("sandbox-proxy denied:"));
        }
        proxy.stop().await;
    }

    #[tokio::test]
    async fn enforced_mode_uses_pinned_request_clients() {
        // redirect::Policy::none() + no_proxy() are the contract that keeps
        // DNS pinning meaningful: a stray env proxy cannot bypass the pin and
        // redirects come back to the container for re-validation.
        let uri: hyper::Uri = "http://127.0.0.1:1/path".parse().unwrap();
        assert!(
            forwarding_client(&uri, true).await.is_err(),
            "non-public targets never reach a forwarding client"
        );
    }

    /// WI#395: a container allowed to reach a host with a query-param
    /// credential mapping can force a send failure (here: a closed port on the
    /// mapped host) and read the 502 body. Neither the 502 body nor the
    /// host-side error log may carry the injected secret or its `?<name>=`
    /// pair (reqwest's Display would otherwise echo the full request URL).
    #[tokio::test]
    #[traced_test]
    async fn proxy_send_error_502_omits_injected_query_credential() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SECRET: &str = "qp-secret-1a2b3c+/==";
        // reqwest `.query()` form-encodes the value, so the raw SECRET never
        // appears in the URL-echoing text; the stem does (GN-2).
        const SECRET_STEM: &str = "qp-secret-1a2b3c";
        let closed = closed_loopback_port().await;

        let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
        let mappings = vec![CredentialMapping {
            secret_name: "QP_TOKEN".to_string(),
            location: CredentialLocation::QueryParam {
                name: "api_key".to_string(),
            },
            host_patterns: vec!["127.0.0.1".to_string()],
            path_patterns: Vec::new(),
            optional: false,
        }];
        let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
        let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));

        let proxy = HttpProxy::new(decider, resolver);
        let addr = proxy.start(0).await.unwrap();

        let request = format!(
            "GET http://127.0.0.1:{closed}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        );
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(response.starts_with("HTTP/1.1 502"), "{response}");
        assert!(
            !response.contains(SECRET_STEM),
            "injected secret leaked into the 502 body: {response}"
        );
        assert!(
            !response.contains("api_key="),
            "credential pair leaked into the 502 body: {response}"
        );

        // The same failure must not leak through the host-side error log.
        assert!(logs_contain("Proxy: request failed"));
        assert!(
            !logs_contain(SECRET_STEM) && !logs_contain("api_key="),
            "injected secret leaked into the proxy error log"
        );

        proxy.stop().await;
    }

    /// WI#395 (GN-1): the enforced-mode proxy never follows redirects, so an
    /// upstream 3xx goes back to the container — and a plain-HTTP request to
    /// an always-HTTPS edge answers `301 Location: https://host/path?name=
    /// <form-encoded secret>`, handing the injected query credential back.
    /// Forwarded header values must be scrubbed of both spellings, with the
    /// warn log naming the header only.
    #[tokio::test]
    #[traced_test]
    async fn proxy_response_header_echo_of_injected_credential_is_scrubbed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SECRET: &str = "qp-echo-secret-7a8b9c+/==";
        const SECRET_STEM: &str = "qp-echo-secret-7a8b9c";
        let encoded: String = url::form_urlencoded::byte_serialize(SECRET.as_bytes()).collect();

        // Upstream that answers every request with a redirect echoing the
        // query (Location) plus a raw bearer echo in a second header.
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let encoded_echo = encoded.clone();
        let upstream_task = tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = upstream.accept().await else {
                    return;
                };
                let encoded_this = encoded_echo.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    // Drain the request head before answering.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        conn.read(&mut buf),
                    )
                    .await;
                    let response = format!(
                        "HTTP/1.1 301 Moved Permanently\r\nLocation: https://example.test/mapped?api_key={encoded_this}\r\nX-Auth-Echo: Bearer {SECRET}\r\nLink: <https://example.test/a?api_key={encoded_this}>; rel=\"first\", <https://example.test/b?api_key={encoded_this}>; rel=\"next\"\r\nX-Api-Key-Echo: {SECRET}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = conn.write_all(response.as_bytes()).await;
                });
            }
        });

        let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
        let mappings = vec![CredentialMapping {
            secret_name: "QP_TOKEN".to_string(),
            location: CredentialLocation::QueryParam {
                name: "api_key".to_string(),
            },
            host_patterns: vec!["127.0.0.1".to_string()],
            path_patterns: Vec::new(),
            optional: false,
        }];
        let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
        let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));

        let proxy = HttpProxy::new(decider, resolver);
        let addr = proxy.start(0).await.unwrap();

        let request = format!(
            "GET http://127.0.0.1:{upstream_port}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        );
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(response.starts_with("HTTP/1.1 301"), "{response}");
        assert!(
            !response.contains(SECRET_STEM),
            "echoed secret leaked into forwarded headers: {response}"
        );
        assert!(
            !response.contains(&encoded),
            "echoed encoded secret leaked into forwarded headers: {response}"
        );
        // run-111 HA-6: every echo must be scrubbed, not only the first per
        // header (the two-link `Link` repeats the query once per link) and
        // not only proper-substring matches (`X-Api-Key-Echo`'s whole value
        // is the raw secret). Five echoes are planted: Location, both Link
        // targets, X-Auth-Echo and X-Api-Key-Echo.
        let redactions = response.matches("[REDACTED]").count();
        assert!(
            redactions >= 5,
            "expected every planted echo scrubbed (found {redactions}): {response}"
        );
        assert!(
            response.contains("[REDACTED]"),
            "scrub marker missing from forwarded headers: {response}"
        );
        assert!(logs_contain("scrubbed echoed credential"));
        assert!(
            !logs_contain(SECRET_STEM) && !logs_contain(&encoded),
            "echoed secret leaked into the proxy log"
        );

        proxy.stop().await;
        upstream_task.abort();
    }

    /// WI#395 (run-111 HA-2): 3xx response *bodies* echo the redirect
    /// target URL on common edges (Go `net/http`, Apache, Express), so the
    /// container could read the injected query credential out of the body
    /// even with the header scrub in place. The body must be scrubbed with
    /// the same spellings, the stale upstream Content-Length dropped (hyper
    /// re-frames), and a body that arrives content-encoded refused rather
    /// than forwarded blind.
    #[tokio::test]
    #[traced_test]
    async fn proxy_redirect_body_echo_of_injected_credential_is_scrubbed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SECRET: &str = "qp-body-secret-3c4d5e+/==";
        const SECRET_STEM: &str = "qp-body-secret-3c4d5e";
        let encoded: String = url::form_urlencoded::byte_serialize(SECRET.as_bytes()).collect();

        // Upstream answering with a Go-style redirect whose body echoes the
        // target URL (the container asked for gzip; the proxy forces
        // identity on credential-injected exchanges).
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let encoded_echo = encoded.clone();
        let upstream_task = tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = upstream.accept().await else {
                    return;
                };
                let encoded_this = encoded_echo.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        conn.read(&mut buf),
                    )
                    .await;
                    let body = format!(
                        "<a href=\"https://example.test/mapped?api_key={encoded_this}\">Found</a>.\n"
                    );
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: https://example.test/mapped?api_key={encoded_this}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(response.as_bytes()).await;
                });
            }
        });

        let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
        let mappings = vec![CredentialMapping {
            secret_name: "QP_TOKEN".to_string(),
            location: CredentialLocation::QueryParam {
                name: "api_key".to_string(),
            },
            host_patterns: vec!["127.0.0.1".to_string()],
            path_patterns: Vec::new(),
            optional: false,
        }];
        let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
        let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));

        let proxy = HttpProxy::new(decider, resolver);
        let addr = proxy.start(0).await.unwrap();

        let request = format!(
            "GET http://127.0.0.1:{upstream_port}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n"
        );
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(response.starts_with("HTTP/1.1 302"), "{response}");
        let (_, forwarded_body) = response
            .split_once("\r\n\r\n")
            .expect("response has a body");
        assert!(
            !forwarded_body.contains(SECRET_STEM) && !forwarded_body.contains(&encoded),
            "echoed secret leaked into the forwarded body: {response}"
        );
        assert!(
            forwarded_body.contains("[REDACTED]"),
            "scrub marker missing from the forwarded body: {response}"
        );
        // The scrubbed body is shorter than the upstream one; a forwarded
        // stale Content-Length would break framing. hyper re-frames from
        // the actual body.
        let content_length = response
            .lines()
            .find_map(|l| {
                l.strip_prefix("Content-Length:")
                    .map(|v| v.trim().to_string())
            })
            .unwrap_or_default();
        let advertised: usize = content_length.parse().unwrap_or(0);
        assert_eq!(
            advertised,
            forwarded_body.len(),
            "Content-Length must match the scrubbed body: {response}"
        );
        assert!(logs_contain(
            "scrubbed echoed credential from response body"
        ));
        assert!(
            !logs_contain(SECRET_STEM) && !logs_contain(&encoded),
            "echoed secret leaked into the proxy log"
        );

        proxy.stop().await;
        upstream_task.abort();
    }

    /// WI#419 (KH-1): the opaque-body check must read every coding field,
    /// split comma-joined tokens, and fail closed on unreadable values.
    /// An `identity` first field must not clear a coding hidden in a later
    /// field (the old `get()` read only the first), and transfer codings
    /// other than `chunked` are opaque.
    #[test]
    fn response_body_coding_is_opaque_checks_all_fields() {
        use http::{HeaderMap, HeaderValue};

        fn with_codings(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                headers.append(*name, HeaderValue::from_str(value).unwrap());
            }
            headers
        }

        assert!(!response_body_coding_is_opaque(&HeaderMap::new()));
        assert!(!response_body_coding_is_opaque(&with_codings(&[(
            "content-encoding",
            "identity"
        ),])));
        assert!(!response_body_coding_is_opaque(&with_codings(&[
            ("content-encoding", "identity"),
            ("transfer-encoding", "chunked"),
        ])));
        // Whitespace-only values carry no coding.
        assert!(!response_body_coding_is_opaque(&with_codings(&[(
            "content-encoding",
            "  "
        ),])));

        // Duplicate fields in either order, and comma-joined tokens.
        for pairs in [
            &[
                ("content-encoding", "identity"),
                ("content-encoding", "gzip"),
            ][..],
            &[
                ("content-encoding", "gzip"),
                ("content-encoding", "identity"),
            ][..],
            &[
                ("content-encoding", "identity, gzip"),
                ("content-encoding", "identity"),
            ][..],
        ] {
            assert!(
                response_body_coding_is_opaque(&with_codings(pairs)),
                "{pairs:?}"
            );
        }

        // Transfer codings other than chunked, in any position or field.
        for pairs in [
            &[("transfer-encoding", "gzip, chunked")][..],
            &[("transfer-encoding", "chunked, gzip")][..],
            &[("transfer-encoding", "identity")][..],
            &[
                ("transfer-encoding", "chunked"),
                ("transfer-encoding", "gzip"),
            ][..],
        ] {
            assert!(
                response_body_coding_is_opaque(&with_codings(pairs)),
                "{pairs:?}"
            );
        }

        // Run-137 IV-1: hyper removes exactly one chunked layer, and only
        // when the last list element of the last field is `chunked`. These
        // spellings leave chunk framing in the body, so the echo scrub
        // could not match them — they must count as opaque.
        for pairs in [
            &[("transfer-encoding", "chunked,")][..],
            &[("transfer-encoding", "chunked"), ("transfer-encoding", "")][..],
            &[("transfer-encoding", "chunked, chunked")][..],
            &[
                ("transfer-encoding", "chunked"),
                ("transfer-encoding", "chunked"),
            ][..],
        ] {
            assert!(
                response_body_coding_is_opaque(&with_codings(pairs)),
                "{pairs:?}"
            );
        }
        // A single `chunked` coding that hyper removes is inspectable,
        // including case and leading-comma variants whose last element is
        // still `chunked`.
        for pairs in [
            &[("transfer-encoding", "chunked")][..],
            &[("transfer-encoding", "Chunked")][..],
            &[("transfer-encoding", ",chunked")][..],
            &[("transfer-encoding", ", chunked")][..],
            &[("transfer-encoding", ""), ("transfer-encoding", "chunked")][..],
        ] {
            assert!(
                !response_body_coding_is_opaque(&with_codings(pairs)),
                "{pairs:?}"
            );
        }

        // An unreadable value is opaque, even beside an identity field.
        let mut headers = HeaderMap::new();
        headers.append(
            "content-encoding",
            HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        headers.append("content-encoding", HeaderValue::from_static("identity"));
        assert!(response_body_coding_is_opaque(&headers));

        // An unreadable Transfer-Encoding is opaque too (the fail-open
        // shape for the transfer check). An empty or whitespace-only TE
        // field carries no coding, but hyper will not de-chunk it either,
        // so it counts as opaque (fail-closed, run-137 IV-1).
        let mut headers = HeaderMap::new();
        headers.append(
            "transfer-encoding",
            HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert!(response_body_coding_is_opaque(&headers));
        assert!(response_body_coding_is_opaque(&with_codings(&[(
            "transfer-encoding",
            ""
        ),])));
        assert!(response_body_coding_is_opaque(&with_codings(&[(
            "transfer-encoding",
            "  "
        ),])));
    }

    /// WI#395 (run-114 HJ-3): pin the forced-identity request and the
    /// opaque-encoding 502 on a credential-injected exchange — a stubborn
    /// gzip upstream that reports the `Accept-Encoding` it received. The
    /// container asks for gzip; the proxy must send `identity` and refuse
    /// the gzip body rather than forward it blind. Pins two previously
    /// untested branches: forwarding the container's own Accept-Encoding,
    /// and disabling the opaque-encoding refusal.
    #[tokio::test]
    #[traced_test]
    async fn proxy_credential_exchange_forces_identity_and_refuses_compressed_body() {
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SECRET: &str = "qp-gzip-secret-4b5c6d+/==";
        const SECRET_STEM: &str = "qp-gzip-secret-4b5c6d";

        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let received: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let recorder = Arc::clone(&received);
        let upstream_task = tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = upstream.accept().await else {
                    return;
                };
                let recorder = Arc::clone(&recorder);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        conn.read(&mut buf),
                    )
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let accept_encoding = head.lines().find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("accept-encoding:")
                            .map(|v| v.trim().to_string())
                    });
                    if let Some(v) = accept_encoding {
                        *recorder.lock().unwrap() = Some(v);
                    }
                    // Gzip magic + opaque payload, regardless of what the
                    // proxy asked for (a stubborn upstream).
                    let body: &[u8] = b"\x1f\x8b\x08opaque-gzip-bytes";
                    let mut out = format!(
                        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    out.extend_from_slice(body);
                    let _ = conn.write_all(&out).await;
                });
            }
        });

        let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
        let mappings = vec![CredentialMapping {
            secret_name: "QP_TOKEN".to_string(),
            location: CredentialLocation::QueryParam {
                name: "api_key".to_string(),
            },
            host_patterns: vec!["127.0.0.1".to_string()],
            path_patterns: Vec::new(),
            optional: false,
        }];
        let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
        let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));

        let proxy = HttpProxy::new(decider, resolver);
        let addr = proxy.start(0).await.unwrap();

        let request = format!(
            "GET http://127.0.0.1:{upstream_port}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept-Encoding: gzip, deflate\r\nConnection: close\r\n\r\n"
        );
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();

        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 502"), "{text}");
        assert!(
            !response.windows(2).any(|w| w == b"\x1f\x8b"),
            "gzip magic must not reach the container: {text}"
        );
        assert!(
            !text.contains(SECRET_STEM) && !text.contains("api_key="),
            "injected credential leaked into the refusal: {text}"
        );
        assert_eq!(
            received.lock().unwrap().as_deref(),
            Some("identity"),
            "the proxy must request identity on the credential-injected exchange"
        );
        assert!(logs_contain(
            "refused content-encoded response body that cannot be scrubbed"
        ));
        assert!(
            !logs_contain(SECRET_STEM) && !logs_contain("api_key="),
            "injected credential leaked into the proxy log"
        );

        proxy.stop().await;
        upstream_task.abort();
    }

    /// WI#419 (KH-1 / run-114 HJ-5): pin the refusal shapes the old
    /// first-field check missed on a credential-injected exchange — a
    /// duplicate `Content-Encoding: identity` + `Content-Encoding: gzip`
    /// answer with a gzip body, a `Transfer-Encoding: gzip, chunked`
    /// answer (hyper removes only the chunk framing), and an unreadable
    /// (non-UTF-8) `Content-Encoding` value. Each must answer 502 with no
    /// gzip bytes and no credential spellings reaching the container.
    #[tokio::test]
    #[traced_test]
    async fn proxy_credential_exchange_refuses_duplicate_transfer_and_unreadable_codings() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SECRET: &str = "qp-multicoding-8e1d4f+/==";
        const SECRET_STEM: &str = "qp-multicoding-8e1d4f";
        const GZIP_BODY: &[u8] = b"\x1f\x8b\x08echo-gzip-bytes";

        let mut duplicate_ce = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Encoding: identity\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            GZIP_BODY.len()
        )
        .into_bytes();
        duplicate_ce.extend_from_slice(GZIP_BODY);

        let mut transfer_coded =
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: gzip, chunked\r\n\r\n"
                .to_vec();
        transfer_coded.extend_from_slice(format!("{:x}\r\n", GZIP_BODY.len()).as_bytes());
        transfer_coded.extend_from_slice(GZIP_BODY);
        transfer_coded.extend_from_slice(b"\r\n0\r\n\r\n");

        let mut unreadable_ce =
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Encoding: \xff\r\nContent-Length: "
                .to_vec();
        unreadable_ce.extend_from_slice(GZIP_BODY.len().to_string().as_bytes());
        unreadable_ce.extend_from_slice(b"\r\n\r\n");
        unreadable_ce.extend_from_slice(GZIP_BODY);

        for upstream_answer in [duplicate_ce, transfer_coded, unreadable_ce] {
            let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream_port = upstream.local_addr().unwrap().port();
            let answer = upstream_answer.clone();
            let upstream_task = tokio::spawn(async move {
                loop {
                    let Ok((mut conn, _)) = upstream.accept().await else {
                        return;
                    };
                    let answer = answer.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 8192];
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            conn.read(&mut buf),
                        )
                        .await;
                        let _ = conn.write_all(&answer).await;
                    });
                }
            });

            let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
            let mappings = vec![CredentialMapping {
                secret_name: "QP_TOKEN".to_string(),
                location: CredentialLocation::QueryParam {
                    name: "api_key".to_string(),
                },
                host_patterns: vec!["127.0.0.1".to_string()],
                path_patterns: Vec::new(),
                optional: false,
            }];
            let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
            let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));

            let proxy = HttpProxy::new(decider, resolver);
            let addr = proxy.start(0).await.unwrap();

            let request = format!(
                "GET http://127.0.0.1:{upstream_port}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n"
            );
            let mut client = TcpStream::connect(addr).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                client.read_to_end(&mut response),
            )
            .await
            .unwrap()
            .unwrap();

            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 502"), "{text}");
            assert!(
                !response.windows(2).any(|w| w == b"\x1f\x8b"),
                "gzip magic must not reach the container: {text}"
            );
            assert!(
                !text.contains(SECRET_STEM) && !text.contains("api_key="),
                "injected credential leaked into the refusal: {text}"
            );

            proxy.stop().await;
            upstream_task.abort();
        }
        assert!(logs_contain(
            "refused content-encoded response body that cannot be scrubbed"
        ));
        assert!(
            !logs_contain(SECRET_STEM) && !logs_contain("api_key="),
            "injected credential leaked into the proxy log"
        );
    }

    /// WI#419: without an injected credential there is nothing to scrub,
    /// so an ordinary exchange keeps the upstream's encoding untouched —
    /// even the duplicate-field and gzip shape the credentialed path
    /// refuses. The refusal is scoped to credential-injected exchanges.
    #[tokio::test]
    async fn proxy_ordinary_exchange_forwards_coded_body_untouched() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const GZIP_BODY: &[u8] = b"\x1f\x8b\x08plain-gzip-bytes";

        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let mut answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Encoding: identity\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            GZIP_BODY.len()
        )
        .into_bytes();
        answer.extend_from_slice(GZIP_BODY);
        let upstream_task = tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = upstream.accept().await else {
                    return;
                };
                let answer = answer.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        conn.read(&mut buf),
                    )
                    .await;
                    let _ = conn.write_all(&answer).await;
                });
            }
        });

        let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
        let decider = Arc::new(DefaultPolicyDecider::new(allowlist, vec![]));
        let proxy = HttpProxy::new(decider, Arc::new(NoCredentialResolver));
        let addr = proxy.start(0).await.unwrap();

        let request = format!(
            "GET http://127.0.0.1:{upstream_port}/plain HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n"
        );
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();

        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("Content-Encoding: gzip"), "{text}");
        assert!(
            response.windows(2).any(|w| w == b"\x1f\x8b"),
            "the ordinary exchange must keep its gzip body: {text}"
        );

        proxy.stop().await;
        upstream_task.abort();
    }

    /// Run-137 IV-1: hyper removes exactly one `chunked` layer, and only
    /// when the last list element of the last field is `chunked`. These
    /// shapes leave chunk framing in the body, so a credential echo split
    /// across chunks is not byte-scrubbed. On a credential-injected exchange
    /// each must answer 502 with neither half forwarded; the single-layer
    /// `chunked`/`Chunked`/`,chunked`/` , chunked` shapes that hyper does
    /// de-chunk must give 200 with exactly `echo=[REDACTED];end` (JD-1/JD-3).
    #[tokio::test]
    #[traced_test]
    async fn proxy_credential_exchange_refuses_chunk_framing_hyper_keeps() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SECRET: &str = "qp-iv1-chunked-3a7b9c+/==";
        const SECRET_STEM: &str = "qp-iv1-chunked-3a7b9c";
        // Split the echo across two chunks: first chunk has the first half
        // of the encoded secret, second chunk has the rest.
        const ECHO_HALF1: &[u8] = b"echo=qp-iv1-chunked-3a7b9c";
        const ECHO_HALF2: &[u8] = b"%2B%2F%3D%3D;end";

        fn chunked_response(te_header: &str, body_parts: &[&[u8]]) -> Vec<u8> {
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: {te_header}\r\n\r\n"
            )
            .into_bytes();
            for part in body_parts {
                out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                out.extend_from_slice(part);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"0\r\n\r\n");
            out
        }

        // Shapes that leave chunk framing in the body → 502, no echo.
        let opaque_answers: Vec<(String, Vec<u8>)> = vec![
            (
                "chunked,".to_string(),
                chunked_response("chunked,", &[ECHO_HALF1, ECHO_HALF2]),
            ),
            (
                "chunked, chunked".to_string(),
                chunked_response("chunked, chunked", &[ECHO_HALF1, ECHO_HALF2]),
            ),
        ];
        // Two-field `chunked` + empty → 502.
        let two_field = {
            let mut out =
                b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n".to_vec();
            out.extend_from_slice(b"Transfer-Encoding: \r\n\r\n");
            out.extend_from_slice(format!("{:x}\r\n", ECHO_HALF1.len()).as_bytes());
            out.extend_from_slice(ECHO_HALF1);
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(format!("{:x}\r\n", ECHO_HALF2.len()).as_bytes());
            out.extend_from_slice(ECHO_HALF2);
            out.extend_from_slice(b"\r\n0\r\n\r\n");
            out
        };
        // Two-field `chunked` + `chunked` (double-chunked) → 502.
        let double_chunked = {
            let mut out =
                b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n".to_vec();
            out.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
            // Inner chunk wraps the echo in chunk framing.
            let inner = {
                let mut i = Vec::new();
                i.extend_from_slice(format!("{:x}\r\n", ECHO_HALF1.len()).as_bytes());
                i.extend_from_slice(ECHO_HALF1);
                i.extend_from_slice(b"\r\n");
                i.extend_from_slice(format!("{:x}\r\n", ECHO_HALF2.len()).as_bytes());
                i.extend_from_slice(ECHO_HALF2);
                i.extend_from_slice(b"\r\n0\r\n\r\n");
                i
            };
            out.extend_from_slice(format!("{:x}\r\n", inner.len()).as_bytes());
            out.extend_from_slice(&inner);
            out.extend_from_slice(b"\r\n0\r\n\r\n");
            out
        };

        let mut opaque_answers = opaque_answers;
        opaque_answers.push(("chunked+empty".to_string(), two_field));
        opaque_answers.push(("chunked+chunked".to_string(), double_chunked));

        for (label, answer) in &opaque_answers {
            let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream_port = upstream.local_addr().unwrap().port();
            let answer = answer.clone();
            let upstream_task = tokio::spawn(async move {
                loop {
                    let Ok((mut conn, _)) = upstream.accept().await else {
                        return;
                    };
                    let answer = answer.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 8192];
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            conn.read(&mut buf),
                        )
                        .await;
                        let _ = conn.write_all(&answer).await;
                    });
                }
            });

            let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
            let mappings = vec![CredentialMapping {
                secret_name: "QP_TOKEN".to_string(),
                location: CredentialLocation::QueryParam {
                    name: "api_key".to_string(),
                },
                host_patterns: vec!["127.0.0.1".to_string()],
                path_patterns: Vec::new(),
                optional: false,
            }];
            let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
            let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));
            let proxy = HttpProxy::new(decider, resolver);
            let addr = proxy.start(0).await.unwrap();

            let request = format!(
                "GET http://127.0.0.1:{upstream_port}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            );
            let mut client = TcpStream::connect(addr).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                client.read_to_end(&mut response),
            )
            .await
            .unwrap()
            .unwrap();

            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 502"), "{label}: {text}");
            assert!(
                !text.contains(SECRET_STEM) && !text.contains("api_key="),
                "{label}: injected credential leaked into the 502: {text}"
            );
            assert!(
                !text.contains("echo=") && !text.contains(";end"),
                "{label}: chunk framing leaked into the 502: {text}"
            );

            proxy.stop().await;
            upstream_task.abort();
        }

        // Single-layer shapes that hyper removes → 200 with the exact scrubbed body.
        for (te_header, label) in [
            ("chunked", "plain chunked"),
            ("Chunked", "Chunked"),
            (",chunked", "leading-comma chunked"),
            (" , chunked", "whitespace leading-comma chunked"),
        ] {
            let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream_port = upstream.local_addr().unwrap().port();
            // Split the echo across two chunks: the scrub can only match it
            // if hyper really removed the framing (run-141 JD-1).
            let answer = chunked_response(te_header, &[ECHO_HALF1, ECHO_HALF2]);
            let upstream_task = tokio::spawn(async move {
                loop {
                    let Ok((mut conn, _)) = upstream.accept().await else {
                        return;
                    };
                    let answer = answer.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 8192];
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            conn.read(&mut buf),
                        )
                        .await;
                        let _ = conn.write_all(&answer).await;
                    });
                }
            });

            let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
            let mappings = vec![CredentialMapping {
                secret_name: "QP_TOKEN".to_string(),
                location: CredentialLocation::QueryParam {
                    name: "api_key".to_string(),
                },
                host_patterns: vec!["127.0.0.1".to_string()],
                path_patterns: Vec::new(),
                optional: false,
            }];
            let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
            let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));
            let proxy = HttpProxy::new(decider, resolver);
            let addr = proxy.start(0).await.unwrap();

            let request = format!(
                "GET http://127.0.0.1:{upstream_port}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            );
            let mut client = TcpStream::connect(addr).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                client.read_to_end(&mut response),
            )
            .await
            .unwrap()
            .unwrap();

            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 200"), "{label}: {text}");
            assert_eq!(
                text.split_once("\r\n\r\n").unwrap().1,
                "echo=[REDACTED];end",
                "{label}: chunk framing kept or echo not scrubbed: {text}"
            );
            assert!(
                !text.contains(SECRET_STEM),
                "{label}: secret survived scrub: {text}"
            );

            proxy.stop().await;
            upstream_task.abort();
        }
        assert!(logs_contain(
            "refused content-encoded response body that cannot be scrubbed"
        ));
        assert!(
            !logs_contain(SECRET_STEM) && !logs_contain("api_key="),
            "injected credential leaked into the proxy log"
        );
    }

    /// WI#395 (run-111 HA-6): a body-read error (upstream promises
    /// Content-Length: 100, sends 5 bytes, closes) carries the request URL
    /// in reqwest's Display — including the injected query credential. The
    /// `without_url()` on the body-read log line must strip it.
    #[tokio::test]
    #[traced_test]
    async fn proxy_body_read_error_log_omits_injected_query_credential() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SECRET: &str = "qp-shortbody-9f8e7d+/==";
        const SECRET_STEM: &str = "qp-shortbody-9f8e7d";

        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let upstream_task = tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = upstream.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        conn.read(&mut buf),
                    )
                    .await;
                    // Promise 100 bytes, deliver 5, close: reqwest's
                    // `bytes()` errors with the URL attached.
                    let _ = conn
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nSHORT",
                        )
                        .await;
                });
            }
        });

        let allowlist = DomainAllowlist::new(&["127.0.0.1".to_string()]);
        let mappings = vec![CredentialMapping {
            secret_name: "QP_TOKEN".to_string(),
            location: CredentialLocation::QueryParam {
                name: "api_key".to_string(),
            },
            host_patterns: vec!["127.0.0.1".to_string()],
            path_patterns: Vec::new(),
            optional: false,
        }];
        let decider = Arc::new(DefaultPolicyDecider::new(allowlist, mappings));
        let resolver = Arc::new(FixedCredentialResolver(SECRET.to_string()));

        let proxy = HttpProxy::new(decider, resolver);
        let addr = proxy.start(0).await.unwrap();

        let request = format!(
            "GET http://127.0.0.1:{upstream_port}/mapped HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        );
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(response.starts_with("HTTP/1.1 502"), "{response}");
        assert!(
            !response.contains(SECRET_STEM) && !response.contains("api_key="),
            "injected credential leaked into the 502 body: {response}"
        );
        assert!(logs_contain("Proxy: failed to read response body"));
        assert!(
            !logs_contain(SECRET_STEM) && !logs_contain("api_key="),
            "injected credential leaked into the body-read error log"
        );

        proxy.stop().await;
        upstream_task.abort();
    }
}
