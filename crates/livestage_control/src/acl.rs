//! IP allowlist for OSC (unauthenticated UDP): which hosts may send.

use std::net::IpAddr;

/// Addresses and CIDR ranges allowed to send. This computer (loopback) is
/// always allowed; an empty list allows no other host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpAllowlist {
    nets: Vec<(IpAddr, u8)>,
}

impl IpAllowlist {
    /// Parses `192.168.1.20`, `10.0.0.0/24`, `fd00::/8`, ... Entries that do
    /// not parse are skipped and returned as errors (the list fails closed).
    pub fn parse(entries: &[String]) -> (Self, Vec<String>) {
        let mut nets = Vec::new();
        let mut errors = Vec::new();
        for raw in entries {
            let entry = raw.trim();
            if entry.is_empty() {
                continue;
            }
            let (addr, bits) = match entry.split_once('/') {
                Some((a, b)) => (a.trim(), b.trim().parse::<u8>().ok()),
                None => (entry, None),
            };
            let Ok(ip) = addr.parse::<IpAddr>() else {
                errors.push(format!("`{entry}` is not an IP address or CIDR range"));
                continue;
            };
            let ip = canonical(ip);
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let bits = match (entry.contains('/'), bits) {
                (false, _) => max,
                (true, Some(b)) if b <= max => b,
                _ => {
                    errors.push(format!("`{entry}` has an invalid prefix length"));
                    continue;
                }
            };
            nets.push((ip, bits));
        }
        (Self { nets }, errors)
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        ip.is_loopback() || self.nets.iter().any(|&(net, bits)| contains(net, bits, ip))
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }
}

/// IPv4-mapped IPv6 (`::ffff:a.b.c.d`) compares as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn contains(net: IpAddr, bits: u8, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(a)) => {
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            u32::from(n) & mask == u32::from(a) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(a)) => {
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
            u128::from(n) & mask == u128::from(a) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn addresses_ranges_and_loopback() {
        let entries: Vec<String> = ["192.168.1.20", "10.0.0.0/24", " fd00::/8 ", "nonsense", "10.1.0.0/33", ""].map(String::from).to_vec();
        let (acl, errors) = IpAllowlist::parse(&entries);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(acl.allows(ip("192.168.1.20")));
        assert!(!acl.allows(ip("192.168.1.21")));
        assert!(acl.allows(ip("10.0.0.255")));
        assert!(!acl.allows(ip("10.0.1.0")));
        assert!(acl.allows(ip("fd12::1")));
        assert!(!acl.allows(ip("fe80::1")));
        assert!(acl.allows(ip("::ffff:10.0.0.7")), "IPv4-mapped IPv6");
        assert!(acl.allows(ip("127.0.0.1")) && acl.allows(ip("::1")), "this computer");
    }

    #[test]
    fn empty_list_allows_only_this_computer() {
        let (acl, _) = IpAllowlist::parse(&[]);
        assert!(acl.is_empty());
        assert!(acl.allows(ip("127.0.0.1")));
        assert!(!acl.allows(ip("192.168.0.2")));
        let (any, _) = IpAllowlist::parse(&["0.0.0.0/0".to_string()]);
        assert!(any.allows(ip("8.8.8.8")), "an explicit catch-all is honoured");
        assert!(!any.allows(ip("2001:db8::1")), "IPv4 range does not cover IPv6");
    }
}
