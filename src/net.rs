use std::net::Ipv4Addr;

use nix::ifaddrs::getifaddrs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolveIpError {
    #[error("interface '{0}' not found")]
    InterfaceNotFound(String),
    #[error("interface '{0}' has no IPv4 address")]
    NoIpv4(String),
    #[error("failed to enumerate interfaces: {0}")]
    Getifaddrs(#[from] nix::Error),
}

/// Resolve the first IPv4 address assigned to the named network interface.
pub fn resolve_ipv4(interface: &str) -> Result<Ipv4Addr, ResolveIpError> {
    let mut found_iface = false;
    for ifa in getifaddrs().map_err(ResolveIpError::Getifaddrs)? {
        if ifa.interface_name != interface {
            continue;
        }
        found_iface = true;
        if let Some(addr) = ifa.address {
            if let Some(sin) = addr.as_sockaddr_in() {
                return Ok(sin.ip());
            }
        }
    }
    if found_iface {
        Err(ResolveIpError::NoIpv4(interface.to_string()))
    } else {
        Err(ResolveIpError::InterfaceNotFound(interface.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_loopback_returns_127_0_0_1() {
        let addr = resolve_ipv4("lo").expect("loopback should always exist");
        assert_eq!(addr, Ipv4Addr::new(127, 0, 0, 1));
    }

    #[test]
    fn resolve_unknown_interface_returns_not_found() {
        let err = resolve_ipv4("nonexistent9999").unwrap_err();
        assert!(
            matches!(err, ResolveIpError::InterfaceNotFound(_)),
            "expected InterfaceNotFound, got: {err}"
        );
    }
}
