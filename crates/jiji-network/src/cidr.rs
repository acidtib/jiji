use crate::NetworkPlanError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Ipv4Cidr {
    network: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Cidr {
    pub fn new(network: Ipv4Addr, prefix: u8) -> Result<Self, NetworkPlanError> {
        if prefix > 32 {
            return Err(NetworkPlanError::InvalidCidr {
                value: format!("{network}/{prefix}"),
                reason: "IPv4 prefixes must be between 0 and 32".to_string(),
            });
        }

        let mask = prefix_mask(prefix);
        if u32::from(network) & mask != u32::from(network) {
            return Err(NetworkPlanError::InvalidCidr {
                value: format!("{network}/{prefix}"),
                reason: "host bits must be zero; specify the network address".to_string(),
            });
        }

        Ok(Self { network, prefix })
    }

    pub fn network(self) -> Ipv4Addr {
        self.network
    }

    pub fn prefix(self) -> u8 {
        self.prefix
    }

    pub fn address_count(self) -> u64 {
        1_u64 << (32 - self.prefix)
    }

    pub fn address(self, offset: u64) -> Option<Ipv4Addr> {
        if offset >= self.address_count() {
            return None;
        }
        Some(Ipv4Addr::from(u32::from(self.network) + offset as u32))
    }

    pub fn contains(self, address: Ipv4Addr) -> bool {
        u32::from(address) & prefix_mask(self.prefix) == u32::from(self.network)
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.contains(other.network) || other.contains(self.network)
    }

    pub fn subnet_count(self, new_prefix: u8) -> Option<u64> {
        if new_prefix < self.prefix || new_prefix > 32 {
            return None;
        }
        Some(1_u64 << (new_prefix - self.prefix))
    }

    pub fn subnet(self, new_prefix: u8, index: u64) -> Option<Self> {
        let count = self.subnet_count(new_prefix)?;
        if index >= count {
            return None;
        }

        let subnet_size = 1_u64 << (32 - new_prefix);
        let address = self.address(index * subnet_size)?;
        Self::new(address, new_prefix).ok()
    }
}

impl fmt::Display for Ipv4Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

impl FromStr for Ipv4Cidr {
    type Err = NetworkPlanError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (address, prefix) =
            value
                .split_once('/')
                .ok_or_else(|| NetworkPlanError::InvalidCidr {
                    value: value.to_string(),
                    reason: "expected IPv4 CIDR notation such as 10.0.0.0/16".to_string(),
                })?;
        let address =
            address
                .parse::<Ipv4Addr>()
                .map_err(|error| NetworkPlanError::InvalidCidr {
                    value: value.to_string(),
                    reason: error.to_string(),
                })?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|error| NetworkPlanError::InvalidCidr {
                value: value.to_string(),
                reason: error.to_string(),
            })?;
        Self::new(address, prefix)
    }
}

fn prefix_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_splits_networks() {
        let cidr: Ipv4Cidr = "10.0.0.0/16".parse().unwrap();
        assert_eq!(cidr.subnet_count(20), Some(16));
        assert_eq!(cidr.subnet(20, 2).unwrap().to_string(), "10.0.32.0/20");
    }

    #[test]
    fn rejects_non_canonical_network_address() {
        let error = "10.0.0.1/16".parse::<Ipv4Cidr>().unwrap_err();
        assert!(error.to_string().contains("host bits must be zero"));
    }
}
