//! Public-only, DNS-pinned egress for enforced proxy mode. Independent of the
//! daemon's ALLOW_PRIVATE_IPS setting (which may be needed for its database).

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

pub(super) async fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| "DNS lookup timed out".to_string())?
    .map_err(|_| "DNS lookup failed".to_string())?
    .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !is_public(a.ip())) {
        return Err("proxy-enforced forbids non-public destination addresses".into());
    }
    Ok(addresses)
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || octets[0] == 0
                || octets[0] >= 240
                || (octets[0] == 100 && (64..128).contains(&octets[1])))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            // Only globally-routed unicast; also exclude transition mechanisms
            // that embed another destination (Teredo / 6to4).
            (ip.segments()[0] & 0xe000) == 0x2000
                && ip.segments()[0] != 0x2002
                && !(ip.segments()[0] == 0x2001 && ip.segments()[1] == 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enforced_proxy_rejects_private_and_mapped_addresses() {
        for host in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fd00::1",
            "fe80::1",
            "0.0.0.0",
        ] {
            assert!(resolve_public(host, 80).await.is_err(), "{host}");
        }
        assert!(is_public("1.1.1.1".parse().unwrap()));
        assert!(is_public("2606:4700:4700::1111".parse().unwrap()));
    }
}
