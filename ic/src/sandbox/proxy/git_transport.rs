//! Git smart-HTTP transport checks for the per-call git proxy (WI#860, ALM-1).
//!
//! The command-text preflight in [`crate::sandbox::git_grant`] cannot see
//! repository configuration, aliases or non-git clients. For plain-HTTP
//! remotes the write and deletion decisions are therefore made here, where
//! the push actually happens:
//!
//! - `git-receive-pack` (the advertisement
//!   `GET <repo>/info/refs?service=git-receive-pack` and
//!   `POST <repo>/git-receive-pack`) is allowed only at a write-granted
//!   remote's own endpoints. Any other request that names receive-pack, in
//!   any percent-encoding or through its `Content-Type`, is refused.
//! - An allowed receive-pack `POST` is refused when its command list deletes
//!   a ref (an all-zero new object id), when it is a signed push
//!   (`push-cert`), and when its body is compressed, truncated or malformed.
//! - Only `GET`, `HEAD`, `POST` and `OPTIONS` are forwarded, on any path:
//!   smart HTTP needs nothing else, and WebDAV writes (`git http-push`) need
//!   the rest.
//! - The upstream `Host` is the request's URI authority: a request whose
//!   `Host` header names anything else is refused, so a client cannot steer
//!   a granted request (or its credential) to another virtual host.
//! - A bound credential accompanies only a request to the bound remote's own
//!   smart-HTTP endpoints (exact scheme, host, port, path, query and method),
//!   and it is that remote's own credential.
//! - `CONNECT` is allowed only to the host and port of a granted `https://`
//!   remote, so a client cannot tunnel around these checks.
//!
//! A non-fast-forward update cannot be seen from the request alone; it is
//! left to the forge's branch protection. HTTPS remotes travel in opaque
//! `CONNECT` tunnels and carry no credential, so the request checks do not
//! apply inside them.

/// One plain-HTTP remote selected for a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTransportRemote {
    /// Lowercase host, without IPv6 brackets or a trailing dot.
    pub host: String,
    pub port: u16,
    /// Normalized repository path without a trailing slash, e.g. `/org/repo.git`.
    pub repo_path: String,
    /// The host granted this remote the write operation class.
    pub write: bool,
    /// The daemon variable bound to this remote (`SANDBOX_GIT_CREDENTIALS`),
    /// if any. Only this credential may accompany this remote's requests.
    pub secret_env: Option<String>,
}

impl GitTransportRemote {
    /// Build from a granted `http://` remote URL; `None` for any other scheme.
    pub fn from_http_url(raw: &str, write: bool, secret_env: Option<String>) -> Option<Self> {
        let url = url::Url::parse(raw).ok()?;
        if url.scheme() != "http" {
            return None;
        }
        let host = normalize_host(url.host_str()?);
        let port = url.port_or_known_default()?;
        let repo_path = url.path().trim_end_matches('/').to_string();
        Some(Self {
            host,
            port,
            repo_path,
            write,
            secret_env,
        })
    }

    /// The request paths of this remote's smart-HTTP endpoints, used to scope
    /// a bound credential. Receive-pack is listed only for a write grant.
    pub fn credential_paths(&self) -> Vec<String> {
        let mut paths = vec![
            format!("{}/info/refs", self.repo_path),
            format!("{}/git-upload-pack", self.repo_path),
        ];
        if self.write {
            paths.push(format!("{}/git-receive-pack", self.repo_path));
        }
        paths
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    UploadPack,
    ReceivePack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    InfoRefs(Service),
    UploadPack,
    ReceivePack,
}

impl Endpoint {
    fn is_receive_pack(self) -> bool {
        matches!(
            self,
            Self::InfoRefs(Service::ReceivePack) | Self::ReceivePack
        )
    }

    fn accepts(self, method: &str) -> bool {
        match self {
            Self::InfoRefs(_) => matches!(method, "GET" | "HEAD"),
            Self::UploadPack | Self::ReceivePack => method == "POST",
        }
    }
}

/// The smart-HTTP transport policy of one per-call git proxy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitTransportPolicy {
    remotes: Vec<GitTransportRemote>,
    /// `host:port` of each granted `https://` remote: the only `CONNECT`
    /// targets this proxy opens.
    tunnels: Vec<(String, u16)>,
}

impl GitTransportPolicy {
    pub fn new(remotes: Vec<GitTransportRemote>) -> Self {
        Self {
            remotes,
            tunnels: Vec::new(),
        }
    }

    /// Allow `CONNECT` to the host and port of a granted `https://` remote.
    /// Any other URL is ignored, so it opens no tunnel.
    pub fn with_https_tunnel(mut self, raw: &str) -> Self {
        if let Ok(url) = url::Url::parse(raw)
            && url.scheme() == "https"
            && let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default())
        {
            let target = (normalize_host(host), port);
            if !self.tunnels.contains(&target) {
                self.tunnels.push(target);
            }
        }
        self
    }

    pub fn remotes(&self) -> &[GitTransportRemote] {
        &self.remotes
    }

    /// Check a forwarded (non-`CONNECT`) request before any policy decision,
    /// credential or network work. `method` is upper case.
    pub fn check_request(
        &self,
        method: &str,
        url: &url::Url,
        content_type: Option<&str>,
    ) -> Result<(), String> {
        if !matches!(method, "GET" | "HEAD" | "POST" | "OPTIONS") {
            return Err(format!("{method} is refused by the git proxy"));
        }
        if let Some((remote, endpoint)) = self.endpoint(url) {
            if !endpoint.accepts(method) {
                return Err(format!("git endpoint does not accept {method}"));
            }
            if endpoint.is_receive_pack() && !remote.write {
                return Err(
                    "git push is not permitted: this remote is read-only in the \
                     tenant's git grant"
                        .to_string(),
                );
            }
            return Ok(());
        }
        if names_receive_pack(url) || content_type.is_some_and(text_names_receive_pack) {
            return Err(
                "git push is permitted only to a write-granted remote's own receive-pack endpoint"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// The upstream `Host` must be the URI authority. `values` are every
    /// `Host` header of the request (an unreadable one as an empty string);
    /// none is fine, the upstream request then takes the URI authority.
    pub fn check_host_header(&self, url: &url::Url, values: &[String]) -> Result<(), String> {
        match values {
            [] => Ok(()),
            [value] if host_header_matches(url, value) => Ok(()),
            [_] => Err("Host header does not match the request target".to_string()),
            _ => Err("more than one Host header".to_string()),
        }
    }

    /// The bound credential that may accompany this request: the variable
    /// bound to the remote whose exact host, port and endpoint path the URL
    /// names, for the method git uses there. `None` for anything else.
    pub fn credential_for(&self, method: &str, url: &url::Url) -> Option<&str> {
        let (remote, endpoint) = self.endpoint(url)?;
        let allowed = endpoint.accepts(method) && (remote.write || !endpoint.is_receive_pack());
        if allowed {
            remote.secret_env.as_deref()
        } else {
            None
        }
    }

    /// `CONNECT` is opened only to the host and port of a granted `https://`
    /// remote. A tunnel anywhere else, a plain-HTTP remote's host included,
    /// would carry traffic past every check here.
    pub fn check_connect(&self, host: &str, port: u16) -> Result<(), String> {
        let target = (normalize_host(host), port);
        if self.tunnels.contains(&target) {
            return Ok(());
        }
        Err("CONNECT is allowed only to a granted https:// git remote".to_string())
    }

    /// Check the body of a forwarded request. Only a receive-pack `POST` to a
    /// granted endpoint is inspected; everything else passes unchanged.
    pub fn check_body(
        &self,
        method: &str,
        url: &url::Url,
        content_encoding: Option<&str>,
        body: &[u8],
    ) -> Result<(), String> {
        let Some((_, Endpoint::ReceivePack)) = self.endpoint(url) else {
            return Ok(());
        };
        if method != "POST" {
            return Ok(());
        }
        // Every coding (several headers arrive joined by commas) must be
        // identity: git never compresses a push request, and a compressed
        // command list cannot be read here.
        if let Some(encoding) = content_encoding
            && encoding.split(',').any(|coding| {
                let coding = coding.trim().to_ascii_lowercase();
                !coding.is_empty() && coding != "identity"
            })
        {
            return Err(
                "compressed git push request bodies are refused by the git proxy".to_string(),
            );
        }
        check_receive_pack_commands(body)
    }

    /// The granted remote and endpoint `url` names exactly, if any.
    fn endpoint(&self, url: &url::Url) -> Option<(&GitTransportRemote, Endpoint)> {
        if url.scheme() != "http" {
            return None;
        }
        let host = normalize_host(url.host_str()?);
        let port = url.port_or_known_default()?;
        let path = url.path();
        self.remotes
            .iter()
            .filter(|remote| remote.host == host && remote.port == port)
            .find_map(|remote| {
                let rest = path.strip_prefix(remote.repo_path.as_str())?;
                let endpoint = match rest {
                    "/info/refs" => Endpoint::InfoRefs(info_refs_service(url)?),
                    "/git-upload-pack" if url.query().is_none() => Endpoint::UploadPack,
                    "/git-receive-pack" if url.query().is_none() => Endpoint::ReceivePack,
                    _ => return None,
                };
                Some((remote, endpoint))
            })
    }
}

/// Whether a `Host` header value names the URL's own authority: the same
/// host (case-insensitive, without IPv6 brackets or a trailing dot) and the
/// same port, an absent port meaning the scheme's default.
fn host_header_matches(url: &url::Url, value: &str) -> bool {
    let Some(url_host) = url.host_str() else {
        return false;
    };
    let Some(url_port) = url.port_or_known_default() else {
        return false;
    };
    let default_port = if url.scheme() == "https" { 443 } else { 80 };
    let value = value.trim();
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let Some((host, after)) = rest.split_once(']') else {
            return false;
        };
        let port = match after {
            "" => Some(default_port),
            after => after
                .strip_prefix(':')
                .and_then(|port| port.parse::<u16>().ok()),
        };
        (host, port)
    } else {
        match value.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().ok()),
            None => (value, Some(default_port)),
        }
    };
    !host.is_empty() && port == Some(url_port) && normalize_host(host) == normalize_host(url_host)
}

/// `service=` of an `info/refs` request; anything but exactly one known
/// service parameter is not a smart-HTTP advertisement.
fn info_refs_service(url: &url::Url) -> Option<Service> {
    let mut service = None;
    for (key, value) in url.query_pairs() {
        if key != "service" || service.is_some() {
            return None;
        }
        service = Some(match value.as_ref() {
            "git-upload-pack" => Service::UploadPack,
            "git-receive-pack" => Service::ReceivePack,
            _ => return None,
        });
    }
    service
}

/// Whether a request target names receive-pack in any percent-encoding.
fn names_receive_pack(url: &url::Url) -> bool {
    let target = format!("{}?{}", url.path(), url.query().unwrap_or(""));
    text_names_receive_pack(&target)
}

/// Case-insensitive `receive-pack` after repeated percent-decoding.
fn text_names_receive_pack(text: &str) -> bool {
    let mut current = text.to_string();
    for _ in 0..4 {
        let decoded = percent_decode_lossy(&current);
        if decoded == current {
            break;
        }
        current = decoded;
    }
    current.to_ascii_lowercase().contains("receive-pack")
}

fn percent_decode_lossy(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(index + 1).copied().and_then(hex_value),
                bytes.get(index + 2).copied().and_then(hex_value),
            )
        {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn normalize_host(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Read the command section of a receive-pack request (pkt-lines up to the
/// first flush-pkt) and refuse ref deletion, signed pushes and malformed or
/// truncated input. The pack data after the flush-pkt is not read.
fn check_receive_pack_commands(body: &[u8]) -> Result<(), String> {
    let mut position = 0usize;
    let mut commands = 0usize;
    loop {
        let header = body
            .get(position..position + 4)
            .ok_or_else(|| "truncated git push request".to_string())?;
        let length =
            parse_pkt_length(header).ok_or_else(|| "malformed git push request".to_string())?;
        if length == 0 {
            // Flush-pkt: the end of the command list.
            return Ok(());
        }
        if length < 4 {
            return Err("malformed git push request".to_string());
        }
        let payload = body
            .get(position + 4..position + length)
            .ok_or_else(|| "truncated git push request".to_string())?;
        position += length;
        let line = payload.strip_suffix(b"\n").unwrap_or(payload);
        if commands == 0 && line.starts_with(b"push-cert") {
            return Err("signed git pushes are not supported through the git proxy".to_string());
        }
        if commands == 0 && line.starts_with(b"shallow ") {
            continue;
        }
        let command = match line.iter().position(|&byte| byte == 0) {
            Some(nul) => &line[..nul],
            None => line,
        };
        let mut parts = command.splitn(3, |&byte| byte == b' ');
        let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err("malformed git push command".to_string());
        };
        if !is_object_id(old) || !is_object_id(new) || old.len() != new.len() || name.is_empty() {
            return Err("malformed git push command".to_string());
        }
        if new.iter().all(|&byte| byte == b'0') {
            return Err("ref deletion is not permitted by the git proxy".to_string());
        }
        commands += 1;
    }
}

fn parse_pkt_length(header: &[u8]) -> Option<usize> {
    header.iter().try_fold(0usize, |length, &byte| {
        hex_value(byte).map(|digit| length * 16 + usize::from(digit))
    })
}

/// A SHA-1 (40) or SHA-256 (64) object id in hex.
fn is_object_id(text: &[u8]) -> bool {
    matches!(text.len(), 40 | 64) && text.iter().all(u8::is_ascii_hexdigit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "1111111111111111111111111111111111111111";
    const NEW: &str = "2222222222222222222222222222222222222222";
    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn secret(name: &str) -> Option<String> {
        Some(name.to_string())
    }

    fn policy() -> GitTransportPolicy {
        GitTransportPolicy::new(vec![
            GitTransportRemote::from_http_url(
                "http://git.internal/org/write.git",
                true,
                secret("WRITE_TOKEN"),
            )
            .unwrap(),
            GitTransportRemote::from_http_url(
                "http://git.internal/org/read.git/",
                false,
                secret("READ_TOKEN"),
            )
            .unwrap(),
            GitTransportRemote::from_http_url("http://git.internal:8080/org/plain", false, None)
                .unwrap(),
        ])
        .with_https_tunnel("https://forge.example.test/org/other.git")
    }

    fn url(raw: &str) -> url::Url {
        url::Url::parse(raw).unwrap()
    }

    fn pkt(line: &str) -> Vec<u8> {
        let mut out = format!("{:04x}", line.len() + 4).into_bytes();
        out.extend_from_slice(line.as_bytes());
        out
    }

    fn push_body(lines: &[String]) -> Vec<u8> {
        let mut body = Vec::new();
        for line in lines {
            body.extend(pkt(line));
        }
        body.extend_from_slice(b"0000PACK....");
        body
    }

    #[test]
    fn remote_parsing_normalizes_host_port_and_path() {
        let remote = GitTransportRemote::from_http_url(
            "http://Git.Internal.:3000/org/repo.git/",
            true,
            None,
        )
        .unwrap();
        assert_eq!(remote.host, "git.internal");
        assert_eq!(remote.port, 3000);
        assert_eq!(remote.repo_path, "/org/repo.git");
        assert!(
            GitTransportRemote::from_http_url("https://git.internal/org/repo.git", true, None)
                .is_none()
        );
        assert_eq!(
            remote.credential_paths(),
            vec![
                "/org/repo.git/info/refs".to_string(),
                "/org/repo.git/git-upload-pack".to_string(),
                "/org/repo.git/git-receive-pack".to_string(),
            ]
        );
        let read =
            GitTransportRemote::from_http_url("http://git.internal/r.git", false, None).unwrap();
        assert!(
            !read
                .credential_paths()
                .iter()
                .any(|path| path.ends_with("receive-pack"))
        );
    }

    #[test]
    fn receive_pack_is_allowed_only_at_a_write_remote_endpoint() {
        let policy = policy();
        for (method, target) in [
            (
                "GET",
                "http://git.internal/org/write.git/info/refs?service=git-receive-pack",
            ),
            ("POST", "http://git.internal/org/write.git/git-receive-pack"),
            (
                "GET",
                "http://git.internal/org/read.git/info/refs?service=git-upload-pack",
            ),
            ("POST", "http://git.internal/org/read.git/git-upload-pack"),
        ] {
            assert!(
                policy.check_request(method, &url(target), None).is_ok(),
                "{method} {target}"
            );
        }
        for (method, target) in [
            // A read-only remote cannot push, with or without a credential.
            (
                "GET",
                "http://git.internal/org/read.git/info/refs?service=git-receive-pack",
            ),
            ("POST", "http://git.internal/org/read.git/git-receive-pack"),
            (
                "POST",
                "http://git.internal:8080/org/plain/git-receive-pack",
            ),
            // Another repository on the same host.
            ("POST", "http://git.internal/org/other.git/git-receive-pack"),
            (
                "GET",
                "http://git.internal/org/other.git/info/refs?service=git-receive-pack",
            ),
            // Encoded and case spellings of the same service.
            (
                "POST",
                "http://git.internal/org/other.git/git%2Dreceive-pack",
            ),
            (
                "POST",
                "http://git.internal/org/other.git/git%252Dreceive%252Dpack",
            ),
            ("POST", "http://git.internal/org/other.git/GIT-RECEIVE-PACK"),
            (
                "GET",
                "http://git.internal/org/other.git/info/refs?service=git%2dreceive-pack",
            ),
            // The write remote on another port is not the granted endpoint.
            (
                "POST",
                "http://git.internal:8081/org/write.git/git-receive-pack",
            ),
            // Wrong method or an extra query parameter.
            ("GET", "http://git.internal/org/write.git/git-receive-pack"),
            (
                "POST",
                "http://git.internal/org/write.git/git-receive-pack?x=1",
            ),
            (
                "GET",
                "http://git.internal/org/write.git/info/refs?service=git-receive-pack&service=git-upload-pack",
            ),
        ] {
            assert!(
                policy.check_request(method, &url(target), None).is_err(),
                "{method} {target}"
            );
        }
        // A receive-pack request body type on any other path is refused too.
        assert!(
            policy
                .check_request(
                    "POST",
                    &url("http://git.internal/org/other.git/upload"),
                    Some("application/x-git-receive-pack-request"),
                )
                .is_err()
        );
    }

    /// F4: smart HTTP needs only GET, HEAD, POST and OPTIONS; every other
    /// method is refused on any path, however the path is spelled.
    #[test]
    fn only_smart_http_methods_are_forwarded_on_any_path() {
        let policy = policy();
        for method in [
            "PUT", "DELETE", "PROPFIND", "MKCOL", "MOVE", "PATCH", "LOCK",
        ] {
            for target in [
                "http://git.internal/org/read.git/objects/info",
                "http://git.internal/org/re%61d.git/objects/info",
                "http://git.internal//org/read.git/objects/info",
                "http://git.internal/elsewhere/file",
            ] {
                assert!(
                    policy.check_request(method, &url(target), None).is_err(),
                    "{method} {target}"
                );
            }
        }
        for method in ["GET", "HEAD", "POST", "OPTIONS"] {
            assert!(
                policy
                    .check_request(method, &url("http://git.internal/elsewhere/file"), None)
                    .is_ok(),
                "{method}"
            );
        }
    }

    /// F1: the upstream Host is the URI authority, so a foreign Host header
    /// cannot steer a granted request to another virtual host.
    #[test]
    fn host_header_must_name_the_uri_authority() {
        let policy = policy();
        let target = url("http://git.internal/org/read.git/git-upload-pack");
        let on_port = url("http://git.internal:8080/org/plain/git-upload-pack");
        let ipv6 = url("http://[::1]:8080/repo.git/git-upload-pack");
        for (url, value) in [
            (&target, "git.internal"),
            (&target, "GIT.Internal"),
            (&target, "git.internal:80"),
            (&target, "git.internal."),
            (&on_port, "git.internal:8080"),
            (&ipv6, "[::1]:8080"),
        ] {
            assert!(
                policy.check_host_header(url, &[value.to_string()]).is_ok(),
                "{url} {value}"
            );
        }
        assert!(policy.check_host_header(&target, &[]).is_ok());
        for (url, value) in [
            (&target, "other.internal"),
            (&target, "git.internal:8080"),
            (&target, ""),
            (&target, "git.internal:"),
            (&on_port, "git.internal"),
            (&ipv6, "[::1]"),
            (&ipv6, "[::2]:8080"),
        ] {
            assert!(
                policy.check_host_header(url, &[value.to_string()]).is_err(),
                "{url} {value}"
            );
        }
        assert!(
            policy
                .check_host_header(
                    &target,
                    &["git.internal".to_string(), "other.internal".to_string()]
                )
                .is_err()
        );
    }

    #[test]
    fn credentials_accompany_only_the_bound_endpoints() {
        let policy = policy();
        assert_eq!(
            policy.credential_for(
                "GET",
                &url("http://git.internal/org/read.git/info/refs?service=git-upload-pack")
            ),
            Some("READ_TOKEN")
        );
        assert_eq!(
            policy.credential_for(
                "POST",
                &url("http://git.internal/org/read.git/git-upload-pack")
            ),
            Some("READ_TOKEN")
        );
        assert_eq!(
            policy.credential_for(
                "POST",
                &url("http://git.internal/org/write.git/git-receive-pack")
            ),
            Some("WRITE_TOKEN")
        );
        for (method, target) in [
            // Another repository, the forge API, another port.
            ("POST", "http://git.internal/org/other.git/git-upload-pack"),
            ("GET", "http://git.internal/api/v4/projects"),
            (
                "POST",
                "http://git.internal:8081/org/read.git/git-upload-pack",
            ),
            // A dumb-protocol read or a path below an endpoint.
            ("GET", "http://git.internal/org/read.git/info/refs"),
            ("GET", "http://git.internal/org/read.git/HEAD"),
            // The wrong method, or receive-pack on a read-only remote.
            ("GET", "http://git.internal/org/read.git/git-upload-pack"),
            ("POST", "http://git.internal/org/read.git/git-receive-pack"),
            // A remote without a binding.
            ("POST", "http://git.internal:8080/org/plain/git-upload-pack"),
        ] {
            assert_eq!(
                policy.credential_for(method, &url(target)),
                None,
                "{method} {target}"
            );
        }
    }

    /// F3: two remotes on one host that differ only by port each get their
    /// own credential, whichever mapping a host-and-path match would pick.
    #[test]
    fn each_remote_gets_its_own_credential_on_a_shared_host() {
        let policy = GitTransportPolicy::new(vec![
            GitTransportRemote::from_http_url("http://h/org/r.git", false, secret("GIT_A"))
                .unwrap(),
            GitTransportRemote::from_http_url("http://h:8080/org/r.git", false, secret("GIT_B"))
                .unwrap(),
        ]);
        assert_eq!(
            policy.credential_for("POST", &url("http://h/org/r.git/git-upload-pack")),
            Some("GIT_A")
        );
        assert_eq!(
            policy.credential_for("POST", &url("http://h:8080/org/r.git/git-upload-pack")),
            Some("GIT_B")
        );
    }

    /// F2: CONNECT opens only to a granted https:// remote's host and port.
    #[test]
    fn connect_is_allowed_only_to_a_granted_https_remote() {
        let policy = policy();
        assert!(policy.check_connect("forge.example.test", 443).is_ok());
        assert!(policy.check_connect("FORGE.example.test.", 443).is_ok());
        for (host, port) in [
            ("forge.example.test", 8443),
            ("git.internal", 80),
            ("git.internal", 8080),
            ("git.internal", 443),
            ("git.internal", 9418),
            ("elsewhere.test", 443),
        ] {
            assert!(policy.check_connect(host, port).is_err(), "{host}:{port}");
        }
        assert!(
            GitTransportPolicy::new(Vec::new())
                .with_https_tunnel("http://forge.example.test/plain.git")
                .check_connect("forge.example.test", 80)
                .is_err()
        );
    }

    #[test]
    fn push_commands_refuse_deletion_signed_and_malformed_bodies() {
        let policy = policy();
        let target = url("http://git.internal/org/write.git/git-receive-pack");
        let update = format!("{OLD} {NEW} refs/heads/main\0report-status side-band-64k");
        let create = format!("{ZERO} {NEW} refs/heads/feature\0report-status");
        assert!(
            policy
                .check_body("POST", &target, None, &push_body(&[update.clone()]))
                .is_ok()
        );
        assert!(
            policy
                .check_body("POST", &target, None, &push_body(&[create]))
                .is_ok()
        );
        assert!(
            policy
                .check_body(
                    "POST",
                    &target,
                    None,
                    &push_body(&[format!("shallow {OLD}"), update.clone()])
                )
                .is_ok()
        );

        let delete = format!("{OLD} {ZERO} refs/heads/old");
        let sha256_delete = format!(
            "{} {} refs/heads/old\0report-status",
            "a".repeat(64),
            "0".repeat(64)
        );
        for body in [
            push_body(&[delete.clone()]),
            push_body(&[update.clone(), delete]),
            push_body(&[sha256_delete]),
            push_body(&["push-cert\0report-status".to_string()]),
            push_body(&[format!("{OLD} {NEW}")]),
            push_body(&[format!("{OLD} {} refs/heads/main", "2".repeat(64))]),
            push_body(&[update.clone(), format!("shallow {OLD}")]),
            b"00".to_vec(),
            b"zzzz".to_vec(),
            b"0003".to_vec(),
            pkt(&update),
        ] {
            assert!(
                policy.check_body("POST", &target, None, &body).is_err(),
                "{}",
                String::from_utf8_lossy(&body)
            );
        }
        // A compressed body cannot be read, so it is refused.
        assert!(
            policy
                .check_body("POST", &target, Some("gzip"), &push_body(&[update.clone()]))
                .is_err()
        );
        assert!(
            policy
                .check_body(
                    "POST",
                    &target,
                    Some("identity, gzip"),
                    &push_body(&[update.clone()])
                )
                .is_err()
        );
        assert!(
            policy
                .check_body(
                    "POST",
                    &target,
                    Some("unreadable"),
                    &push_body(&[update.clone()])
                )
                .is_err()
        );
        assert!(
            policy
                .check_body("POST", &target, Some("identity"), &push_body(&[update]))
                .is_ok()
        );
        // Fetches are not inspected.
        assert!(
            policy
                .check_body(
                    "POST",
                    &url("http://git.internal/org/read.git/git-upload-pack"),
                    Some("gzip"),
                    b"opaque",
                )
                .is_ok()
        );
    }
}
