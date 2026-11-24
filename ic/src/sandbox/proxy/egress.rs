//! Public-only, DNS-pinned egress for enforced proxy mode. Independent of the
//! daemon's ALLOW_PRIVATE_IPS setting (which may be needed for its database).
//!
//! A per-call git proxy may carry an operator allowance for an exact
//! `host:port` destination (`SANDBOX_GIT_PRIVATE_REMOTES`, WI#875). Only that
//! destination may then resolve to a LAN (RFC 1918) or overlay (CGNAT, IPv6
//! ULA) address. Loopback, link-local and metadata addresses stay refused
//! even for an allowed destination, so a name that re-resolves there cannot
//! reach the host's own services or the cloud metadata endpoint. The shared
//! tenant proxy carries no allowance and stays public-only.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Exact `host:port` destinations whose non-public address the operator
/// allowed for this call. Empty means "public destinations only", which is
/// every proxy except a git proxy with an allowance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivateEgress {
    allowed: Vec<(String, u16)>,
}

impl PrivateEgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow one exact destination. `host` is normalized like a grant host:
    /// lowercase, without IPv6 brackets or a trailing dot.
    pub fn allow(mut self, host: &str, port: u16) -> Self {
        let host = normalize_host(host);
        if !self.allowed.iter().any(|(h, p)| *h == host && *p == port) {
            self.allowed.push((host, port));
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    fn contains(&self, host: &str, port: u16) -> bool {
        let host = normalize_host(host);
        self.allowed.iter().any(|(h, p)| *h == host && *p == port)
    }
}

fn normalize_host(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Resolve a destination that must be public, with no operator allowance.
/// The shared tenant proxy's contract; a git proxy calls `resolve_egress`
/// with its grant's allowance instead.
#[cfg(test)]
pub(super) async fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    resolve_egress(host, port, &PrivateEgress::new()).await
}

/// Resolve a destination and pin the result. Every address must be public
/// unless the exact `host:port` is in `allowed`; even then the address must
/// be a private LAN or overlay address, never loopback, link-local or a
/// metadata address.
pub(super) async fn resolve_egress(
    host: &str,
    port: u16,
    allowed: &PrivateEgress,
) -> Result<Vec<SocketAddr>, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| "DNS lookup timed out".to_string())?
    .map_err(|_| "DNS lookup failed".to_string())?
    .collect();
    if addresses.is_empty() {
        return Err("proxy-enforced requires a resolvable destination".into());
    }
    if addresses.iter().any(|a| !is_public(a.ip())) {
        if !allowed.contains(host, port) {
            return Err("proxy-enforced forbids non-public destination addresses".into());
        }
        if addresses.iter().any(|a| !is_private_or_overlay(a.ip())) {
            return Err(
                "proxy-enforced forbids loopback, link-local and metadata destination addresses"
                    .into(),
            );
        }
    }
    Ok(addresses)
}

/// A LAN or overlay address the operator may allow: RFC 1918, the CGNAT
/// range overlay networks use, or an IPv6 unique-local address.
fn is_private_or_overlay(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private() || (octets[0] == 100 && (64..128).contains(&octets[1]))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_private_or_overlay(IpAddr::V4(v4));
            }
            ip.is_unique_local()
        }
    }
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
            // Only globally-routed unicast (2000::/3); exclude transition
            // mechanisms that embed another destination (6to4, Teredo).
            (ip.segments()[0] & 0xe000) == 0x2000
                && ip.segments()[0] != 0x2002
                && !(ip.segments()[0] == 0x2001 && ip.segments()[1] == 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn v4(o: [u8; 4]) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(o))
    }

    #[test]
    fn private_and_special_v4_ranges_are_rejected() {
        for o in [
            [0, 0, 0, 0],
            [10, 0, 0, 1],
            [100, 64, 0, 1],
            [100, 127, 255, 254],
            [127, 0, 0, 1],
            [169, 254, 0, 1],
            [172, 16, 0, 1],
            [172, 31, 255, 255],
            [192, 168, 1, 1],
            [240, 0, 0, 1],
            [255, 255, 255, 255],
        ] {
            assert!(!is_public(v4(o)), "must reject {o:?}");
        }
        for o in [
            [1, 1, 1, 1],
            [8, 8, 8, 8],
            [100, 128, 0, 1],
            [93, 184, 216, 34],
        ] {
            assert!(is_public(v4(o)), "must accept {o:?}");
        }
    }

    #[test]
    fn v6_transition_and_local_never_pass() {
        for ip in [
            "::",
            "::1",
            "fe80::1",
            "ff02::1",
            "2002::1", // 6to4 embeds another address
            "2001::1", // Teredo embeds another address
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(
                !is_public(ip.parse::<IpAddr>().unwrap()),
                "must reject {ip}"
            );
        }
        assert!(is_public("2606:4700:4700::1111".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn spec_v6_bucket_is_used() {
        let _: Ipv6Addr = Ipv6Addr::UNSPECIFIED;
    }

    #[tokio::test]
    async fn loopback_connect_is_never_public() {
        let result = resolve_public("127.0.0.1", 43210).await;
        assert!(result.is_err());
        let v6 = resolve_public("::ffff:127.0.0.1", 43210).await;
        assert!(v6.is_err());
    }

    #[tokio::test]
    async fn localhost_is_never_public() {
        assert!(resolve_public("localhost", 1).await.is_err());
    }

    #[test]
    fn allowance_covers_private_and_overlay_ranges_only() {
        for ip in [
            "10.1.2.3",
            "172.16.5.4",
            "192.168.1.20",
            "100.64.0.7",
            "100.127.255.1",
            "fd00::1",
            "fd12:3456::9",
            "::ffff:10.1.2.3",
        ] {
            assert!(
                is_private_or_overlay(ip.parse::<IpAddr>().unwrap()),
                "must allow {ip}"
            );
        }
        for ip in [
            "127.0.0.1",
            "::1",
            "169.254.169.254",
            "fe80::1",
            "0.0.0.0",
            "224.0.0.1",
            "1.1.1.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(
                !is_private_or_overlay(ip.parse::<IpAddr>().unwrap()),
                "must refuse {ip} even when allowed"
            );
        }
    }

    #[test]
    fn allowance_is_exact_host_and_port() {
        let allowed = PrivateEgress::new()
            .allow("Git.LAN.", 3000)
            .allow("100.64.0.7", 8080);
        assert!(allowed.contains("git.lan", 3000));
        assert!(allowed.contains("GIT.LAN", 3000));
        assert!(allowed.contains("100.64.0.7", 8080));
        assert!(!allowed.contains("git.lan", 3001));
        assert!(!allowed.contains("other.lan", 3000));
        assert!(!allowed.contains("100.64.0.8", 8080));
        assert!(PrivateEgress::new().is_empty());
        assert!(!allowed.is_empty());
    }

    #[tokio::test]
    async fn an_allowed_destination_resolves_but_loopback_still_fails() {
        let allowed = PrivateEgress::new().allow("127.0.0.1", 43210);
        assert!(
            resolve_egress("127.0.0.1", 43210, &allowed).await.is_err(),
            "an allowance never admits loopback"
        );
        assert!(
            resolve_egress("127.0.0.1", 43210, &PrivateEgress::new())
                .await
                .is_err()
        );
        let elsewhere = PrivateEgress::new().allow("127.0.0.1", 43211);
        assert!(
            resolve_egress("127.0.0.1", 43210, &elsewhere).await.is_err(),
            "an allowance for another port admits nothing"
        );
    }
}
