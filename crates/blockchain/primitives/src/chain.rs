//! Outbe chain constants and network identification.
//!
//! Native token: COEN (18 decimals), base unit: unit.

/// Canonical Outbe network identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutbeNetwork {
    Devnet,
    Testnet,
    Mainnet,
}

impl OutbeNetwork {
    /// Returns the configured chain id for networks that are already assigned in-tree.
    pub const fn chain_id(self) -> Option<u64> {
        match self {
            Self::Devnet => Some(DEVNET_CHAIN_ID),
            Self::Testnet => Some(TESTNET_CHAIN_ID),
            Self::Mainnet => None,
        }
    }

    pub const fn chain_name(self) -> &'static str {
        match self {
            Self::Devnet => DEVNET_CHAIN_NAME,
            Self::Testnet => TESTNET_CHAIN_NAME,
            Self::Mainnet => MAINNET_CHAIN_NAME,
        }
    }

    pub const fn is_devnet(self) -> bool {
        matches!(self, Self::Devnet)
    }

    pub const fn is_testnet(self) -> bool {
        matches!(self, Self::Testnet)
    }

    pub const fn is_mainnet(self) -> bool {
        matches!(self, Self::Mainnet)
    }
}

/// Chain ID for outbe-devnet-1.
pub const DEVNET_CHAIN_ID: u64 = 424_242;
/// Chain ID for outbe-testnet-1.
pub const TESTNET_CHAIN_ID: u64 = 54_322_345;

/// Chain name for outbe-devnet-1.
pub const DEVNET_CHAIN_NAME: &str = "outbe-devnet-1";
/// Chain name for outbe-testnet-1.
pub const TESTNET_CHAIN_NAME: &str = "outbe-testnet-1";
/// Chain name for outbe-mainnet-1.
pub const MAINNET_CHAIN_NAME: &str = "outbe-mainnet-1";

/// Default compiled chain ID.
pub const CHAIN_ID: u64 = DEVNET_CHAIN_ID;
/// Default compiled chain name.
pub const CHAIN_NAME: &str = DEVNET_CHAIN_NAME;

/// Resolves a chain id to a known Outbe network.
pub const fn network_for_chain_id(chain_id: u64) -> Option<OutbeNetwork> {
    match chain_id {
        DEVNET_CHAIN_ID => Some(OutbeNetwork::Devnet),
        TESTNET_CHAIN_ID => Some(OutbeNetwork::Testnet),
        _ => None,
    }
}

pub const fn is_devnet(chain_id: u64) -> bool {
    match network_for_chain_id(chain_id) {
        Some(network) => network.is_devnet(),
        None => false,
    }
}

pub const fn is_testnet(chain_id: u64) -> bool {
    match network_for_chain_id(chain_id) {
        Some(network) => network.is_testnet(),
        None => false,
    }
}

pub const fn is_mainnet(chain_id: u64) -> bool {
    match network_for_chain_id(chain_id) {
        Some(network) => network.is_mainnet(),
        None => false,
    }
}
