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
}
