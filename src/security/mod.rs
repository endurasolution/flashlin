pub mod access_control;
pub mod headers;
pub mod rate_limit;
pub mod sensitive_files;

use std::net::{IpAddr, Ipv6Addr};

/// The identity per-client limits are keyed by. IPv4-mapped IPv6 addresses
/// (what a dual-stack `::` listener reports for IPv4 clients) become plain
/// IPv4, and IPv6 clients are grouped by /64 — the smallest block an ISP
/// usually assigns, which a client can otherwise rotate through freely.
pub fn client_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !((1u128 << 64) - 1))),
        v4 => v4,
    }
}
