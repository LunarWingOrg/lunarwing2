use std::collections::HashMap;

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

/// Resolve every address before connecting so an allowlisted hostname cannot
/// resolve to a private target through DNS rebinding.
pub(crate) fn reject_private_ip(url: &str) -> Result<(), String> {
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

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return if is_private_ip(ip) {
            Err(format!(
                "HTTP request to private/internal IP {} is not allowed",
                ip
            ))
        } else {
            Ok(())
        };
    }

    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = format!("{}:0", host)
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed for {}: {}", host, e))?
        .collect();

    if addrs.is_empty() {
        return Err(format!("DNS resolution returned no addresses for {}", host));
    }

    for addr in &addrs {
        if is_private_ip(addr.ip()) {
            return Err(format!(
                "DNS rebinding detected: {} resolved to private IP {}",
                host,
                addr.ip()
            ));
        }
    }

    Ok(())
}

pub(crate) fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xFE00) == 0xFC00
                || (v6.segments()[0] & 0xFFC0) == 0xFE80
        }
    }
}
