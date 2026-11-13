//! External CREATE3 contract addresses owned by `core` domains.

use alloy_primitives::{address, Address};

/// IntexNFT1155: the Intex ERC-1155 series balance ledger, not a precompile. CREATE3 proxy under salt
/// "outbe-intex:IntexNFT1155:v7.0.0", deployer 0x2Af7d3C5…1c05, factory 0xe78edad9…d14b.
#[cfg(not(feature = "e2e-test"))]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x8E5041c14D3f0b802E8eFA4e575E5234265C79a6");
#[cfg(feature = "e2e-test")]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x116063CF0558225D808d1C309322d6C3CbfDE343");

/// Intex OriginRouter: the auction's outbound ERC-7786 sends and target-chain
/// registry. CREATE3 proxy under salt "outbe-intex:OriginRouter:v7.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x62C1EFaCBfB20a864342B61A6dEC6f4d80729D18");
#[cfg(feature = "e2e-test")]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x78639251937A333daee04233867A199E314D7Bbb");
