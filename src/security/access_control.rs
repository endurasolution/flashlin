use std::net::IpAddr;

use crate::config::ServerConfig;
use crate::error::{FlashError, Result};

/// Check whether the connecting IP is allowed by the configured allow/deny rules.
///
/// Rules are evaluated in this order:
/// 1. If `allow_ips` is set and non-empty, the IP must match one entry.
/// 2. If `deny_ips` is set and non-empty, the IP must NOT match any entry.
pub fn check_ip_access(ip: &IpAddr, config: &ServerConfig) -> Result<()> {
    let sec = match &config.security {
        Some(s) => s,
        None => return Ok(()),
    };

    // Allow-list check.
    if let Some(allow) = &sec.allow_ips {
        if !allow.is_empty() {
            let allowed = allow.iter().any(|entry| matches_ip_rule(ip, entry));
            if !allowed {
                return Err(FlashError::AccessDenied);
            }
        }
    }

    // Deny-list check.
    if let Some(deny) = &sec.deny_ips {
        if !deny.is_empty() {
            let denied = deny.iter().any(|entry| matches_ip_rule(ip, entry));
            if denied {
                return Err(FlashError::AccessDenied);
            }
        }
    }

    Ok(())
}

/// Very simple IP / CIDR matcher.
/// Supports: exact IP ("192.168.1.1"), CIDR ("192.168.0.0/24"), wildcard ("192.168.*").
pub fn matches_ip_rule(ip: &IpAddr, rule: &str) -> bool {
    // A dual-stack listener reports IPv4 clients as ::ffff:a.b.c.d, which
    // would otherwise slip past every IPv4 rule.
    let ip = &ip.to_canonical();
    // Wildcard shorthand: "192.168.*"
    if rule.contains('*') {
        let prefix = rule.trim_end_matches(".*").trim_end_matches('*');
        let address = ip.to_string();
        return address == prefix || address.starts_with(&format!("{prefix}."));
    }

    // CIDR notation.
    if let Some((net_str, prefix_len_str)) = rule.split_once('/') {
        if let (Ok(net_ip), Ok(prefix_len)) =
            (net_str.parse::<IpAddr>(), prefix_len_str.parse::<u8>())
        {
            return ip_in_cidr(ip, &net_ip, prefix_len);
        }
    }

    // Exact match.
    rule.parse::<IpAddr>()
        .map(|rule_ip| rule_ip.to_canonical() == *ip)
        .unwrap_or(false)
}

fn ip_in_cidr(ip: &IpAddr, network: &IpAddr, prefix_len: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip4), IpAddr::V4(net4)) => {
            if prefix_len > 32 {
                return false;
            }
            let mask: u32 = if prefix_len == 0 {
                0
            } else {
                !0u32 << (32 - prefix_len)
            };
            (u32::from(*ip4) & mask) == (u32::from(*net4) & mask)
        }
        (IpAddr::V6(ip6), IpAddr::V6(net6)) => {
            if prefix_len > 128 {
                return false;
            }
            let ip_bits = u128::from(*ip6);
            let net_bits = u128::from(*net6);
            let mask: u128 = if prefix_len == 0 {
                0
            } else {
                !0u128 << (128 - prefix_len)
            };
            (ip_bits & mask) == (net_bits & mask)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn exact_allow() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(matches_ip_rule(&ip, "1.2.3.4"));
        assert!(!matches_ip_rule(&ip, "1.2.3.5"));
    }

    #[test]
    fn cidr_match() {
        let ip: IpAddr = "192.168.1.50".parse().unwrap();
        assert!(matches_ip_rule(&ip, "192.168.1.0/24"));
        assert!(!matches_ip_rule(&ip, "192.168.2.0/24"));
    }

    #[test]
    fn ipv4_mapped_ipv6_clients_match_ipv4_rules() {
        let mapped: IpAddr = "::ffff:1.2.3.4".parse().unwrap();
        assert!(matches_ip_rule(&mapped, "1.2.3.4"));
        assert!(matches_ip_rule(&mapped, "1.2.3.0/24"));
        assert!(matches_ip_rule(&mapped, "1.2.*"));
        let mut config = crate::default_config();
        config.security.as_mut().unwrap().deny_ips = Some(vec!["1.2.3.4".into()]);
        assert!(check_ip_access(&mapped, &config).is_err());
    }

    #[test]
    fn wildcard_match() {
        let ip: IpAddr = "10.0.5.1".parse().unwrap();
        assert!(matches_ip_rule(&ip, "10.0.*"));
        assert!(!matches_ip_rule(&ip, "10.1.*"));
    }
}
