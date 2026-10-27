use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use futures::{Stream, StreamExt};

const MIN_CREDENTIAL_SCRUB_LEN: usize = 4;

pub(crate) struct RedactionNeedle {
    pub(crate) value: String,
    pub(crate) marker: String,
}

pub(crate) fn push_credential_needles(
    needles: &mut Vec<RedactionNeedle>,
    secret: &str,
    marker: &str,
) {
    if secret.len() < MIN_CREDENTIAL_SCRUB_LEN {
        return;
    }
    let mut spellings = vec![secret.to_string()];
    spellings.extend(encoded_spellings(secret));
    for value in spellings {
        if value.len() >= MIN_CREDENTIAL_SCRUB_LEN {
            needles.push(RedactionNeedle {
                value,
                marker: marker.to_string(),
            });
        }
    }
}

pub(crate) fn sort_redaction_needles(needles: &mut Vec<RedactionNeedle>) {
    needles.sort_by(|left, right| {
        right
            .value
            .len()
            .cmp(&left.value.len())
            .then_with(|| left.value.cmp(&right.value))
            .then_with(|| left.marker.cmp(&right.marker))
    });
    needles.dedup_by(|left, right| left.value == right.value);
}

pub(crate) fn redact_text_with_needles(text: &str, needles: &[RedactionNeedle]) -> String {
    let mut result = text.to_string();
    for needle in needles {
        result = result.replace(&needle.value, &needle.marker);
    }
    result
}

pub(crate) fn redact_cross_field_echoes(
    fields: &mut HashMap<String, String>,
    needles: &[RedactionNeedle],
) {
    let snapshot: Vec<(String, String)> = fields
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut replacements = HashMap::new();
    for (index, (left_name, left_value)) in snapshot.iter().enumerate() {
        for (right_name, right_value) in snapshot.iter().skip(index + 1) {
            if let Some(needle) = cross_field_needle(left_value, right_value, needles) {
                replacements
                    .entry(left_name.clone())
                    .or_insert_with(|| needle.marker.clone());
                replacements
                    .entry(right_name.clone())
                    .or_insert_with(|| needle.marker.clone());
            }
        }
    }
    for (name, value) in fields {
        if let Some(marker) = replacements.get(name) {
            *value = marker.clone();
        }
    }
}

fn cross_field_needle<'a>(
    left: &str,
    right: &str,
    needles: &'a [RedactionNeedle],
) -> Option<&'a RedactionNeedle> {
    needles.iter().find(|needle| {
        let value = &needle.value;
        // One-character fragments are accepted deliberately: a false positive
        // over-redacts an unrelated neighbour rather than exposing a remainder.
        (1..value.len()).any(|split| {
            value.is_char_boundary(split)
                && ((left.ends_with(&value[..split]) && right.starts_with(&value[split..]))
                    || (right.ends_with(&value[..split]) && left.starts_with(&value[split..])))
        })
    })
}

pub(crate) fn form_encode_secret(secret: &str) -> String {
    url::form_urlencoded::byte_serialize(secret.as_bytes()).collect()
}

pub(crate) fn encoded_spellings(secret: &str) -> Vec<String> {
    let mut out = vec![
        urlencoding::encode(secret).to_string(),
        form_encode_secret(secret),
    ];
    if let Ok(mut parsed) = url::Url::parse("http://redaction.invalid/") {
        parsed.set_query(Some(secret));
        if let Some(query) = parsed.query() {
            out.push(query.to_string());
        }
        parsed.set_query(None);
        parsed.set_path(secret);
        out.push(parsed.path().trim_start_matches('/').to_string());
    }
    // An upstream that parses a raw-substituted query placeholder reads '+'
    // as a space and decodes %XX escapes; echoing the parsed value gives
    // these spellings (run-143 JQ).
    for candidate in [secret.replace('+', " "), secret.to_string()] {
        if let Ok(decoded) = urlencoding::decode(&candidate) {
            out.push(decoded.into_owned());
        }
    }
    if let Ok(json) = serde_json::to_string(secret)
        && let Some(inner) = json.strip_prefix('"').and_then(|v| v.strip_suffix('"'))
    {
        out.push(inner.to_string());
    }
    out.push(hex_encode_secret(secret, false));
    out.push(hex_encode_secret(secret, true));
    out.retain(|spelling| !spelling.is_empty() && spelling != secret);
    out.sort();
    out.dedup();
    out
}

fn hex_encode_secret(secret: &str, uppercase: bool) -> String {
    let digits = if uppercase {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut encoded = String::with_capacity(secret.len() * 2);
    for byte in secret.bytes() {
        encoded.push(digits[(byte >> 4) as usize] as char);
        encoded.push(digits[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(crate) fn replace_all_bytes(haystack: &mut Vec<u8>, needle: &[u8], marker: &[u8]) {
    if needle.is_empty() || haystack.len() < needle.len() {
        return;
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut index = 0;
    let mut changed = false;
    while index < haystack.len() {
        if haystack[index..].starts_with(needle) {
            out.extend_from_slice(marker);
            index += needle.len();
            changed = true;
        } else {
            out.push(haystack[index]);
            index += 1;
        }
    }
    if changed {
        *haystack = out;
    }
}

pub(crate) fn enforce_response_size(size: u64, max_response_bytes: usize) -> Result<(), String> {
    if size > max_response_bytes as u64 {
        return Err(format!(
            "Response body too large: {} bytes exceeds limit of {} bytes",
            size, max_response_bytes
        ));
    }
    Ok(())
}

pub(crate) async fn collect_capped_response_body<S, E>(
    stream: S,
    content_length: Option<u64>,
    max_response_bytes: usize,
) -> Result<Vec<u8>, String>
where
    S: Stream<Item = Result<bytes::Bytes, E>>,
    E: std::fmt::Display,
{
    if let Some(content_length) = content_length {
        enforce_response_size(content_length, max_response_bytes)?;
    }

    let mut body = Vec::new();
    futures::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to read response body: {e}"))?;
        let new_size = (body.len() as u64).saturating_add(chunk.len() as u64);
        enforce_response_size(new_size, max_response_bytes)?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn private_http_targets_allowed() -> bool {
    std::env::var("WASM_HTTP_ALLOW_PRIVATE_IPS").ok().as_deref() == Some("1")
}

/// A public HTTP target and the exact DNS answers vetted for its connection.
pub(crate) struct VettedHttpTarget {
    dns_host: Option<String>,
    addresses: Vec<SocketAddr>,
}

impl VettedHttpTarget {
    /// Pin reqwest to the answers checked above, eliminating the second DNS lookup.
    pub(crate) fn pin_client(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        match &self.dns_host {
            Some(host) => builder.resolve_to_addrs(host, &self.addresses),
            None => builder,
        }
    }
}

/// Resolve and validate every address before connecting. HTTP callers must pass
/// the result to [`VettedHttpTarget::pin_client`] on the same request's client.
pub(crate) fn vet_http_target(url: &str) -> Result<VettedHttpTarget, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Failed to parse URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("Unsupported URL scheme: {}", parsed.scheme()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URL contains userinfo (@) which is not allowed".to_string());
    }

    let host = parsed
        .host_str()
        .map(|h| {
            h.strip_prefix('[')
                .and_then(|v| v.strip_suffix(']'))
                .unwrap_or(h)
        })
        .ok_or_else(|| "Failed to parse host from URL".to_string())?;

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(format!(
                "HTTP request to private/internal IP {ip} is not allowed"
            ));
        }
        return Ok(VettedHttpTarget {
            dns_host: None,
            addresses: Vec::new(),
        });
    }

    use std::net::ToSocketAddrs;
    let addresses: Vec<_> = format!("{host}:0")
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed for {host}: {e}"))?
        .collect();
    if addresses.is_empty() {
        return Err(format!("DNS resolution returned no addresses for {host}"));
    }
    for addr in &addresses {
        if is_private_ip(addr.ip()) {
            return Err(format!(
                "DNS rebinding detected: {host} resolved to private IP {}",
                addr.ip()
            ));
        }
    }

    Ok(VettedHttpTarget {
        dns_host: Some(host.to_string()),
        addresses,
    })
}

pub(crate) fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped().or_else(|| embedded_ipv4(&v6)) {
                return is_private_ipv4(v4);
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xFE00) == 0xFC00
                || (first & 0xFFC0) == 0xFE80
                || (first & 0xFFC0) == 0xFEC0
        }
    }
}

fn is_private_ipv4(v4: Ipv4Addr) -> bool {
    let octets = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_multicast()
        || octets[0] == 0
        || octets[0] == 100 && (octets[1] & 0xC0) == 64
        || octets[..3] == [192, 0, 0]
        || octets[0] == 198 && (octets[1] & 0xFE) == 18
        || octets[0] >= 240
}

fn embedded_ipv4(v6: &std::net::Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = v6.segments();
    let compatible = segments[..6].iter().all(|segment| *segment == 0);
    let nat64 = segments[0] == 0x64
        && segments[1] == 0xFF9B
        && segments[2..6].iter().all(|segment| *segment == 0);
    if !compatible && !nat64 {
        return None;
    }
    let bits = (u32::from(segments[6]) << 16) | u32::from(segments[7]);
    Some(Ipv4Addr::from(bits))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

    use super::{VettedHttpTarget, is_private_ip, vet_http_target};

    #[test]
    fn special_and_embedded_addresses_are_classified_before_connection() {
        let blocked = [
            "0.0.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "198.19.255.255",
            "240.0.0.1",
            "255.255.255.255",
            "224.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "ff02::1",
            "fec0::1",
        ];
        for text in blocked {
            let ip = text.parse::<IpAddr>().expect("test IP");
            assert!(is_private_ip(ip), "{text} must be refused");
        }

        let public = [
            "8.8.8.8",
            "192.0.1.1",
            "198.20.0.1",
            "::ffff:8.8.8.8",
            "::8.8.8.8",
            "64:ff9b::808:808",
            "2606:4700::1111",
        ];
        for text in public {
            let ip = text.parse::<IpAddr>().expect("test IP");
            assert!(!is_private_ip(ip), "{text} must stay allowed");
        }
        assert!(vet_http_target("http://[::ffff:127.0.0.1]:8080/").is_err());
    }

    #[test]
    fn vetted_addresses_are_used_for_the_actual_connection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("write response");
        });
        let target = VettedHttpTarget {
            dns_host: Some("pinned.test".to_string()),
            addresses: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)],
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let status = runtime.block_on(async {
            let client = target
                .pin_client(reqwest::Client::builder().no_proxy())
                .build()
                .expect("client");
            client
                .get(format!("http://pinned.test:{port}/"))
                .send()
                .await
                .expect("pinned request")
                .status()
        });

        assert_eq!(status, reqwest::StatusCode::OK);
        server.join().expect("server thread");
    }
}
