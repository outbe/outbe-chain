//! External CREATE3 contract addresses owned by `core` domains.

use alloy_primitives::{address, Address};

/// IntexNFT1155: the Intex ERC-1155 series balance ledger, not a precompile. CREATE3 proxy under salt
/// "outbe-intex:IntexNFT1155:v8.0.0", deployer 0x2Af7d3C5…1c05, factory 0xe78edad9…d14b.
#[cfg(not(feature = "e2e-test"))]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x00956B1d4F624d1bD86a1f2Bd80ce877FB5C3746");
#[cfg(feature = "e2e-test")]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x116063CF0558225D808d1C309322d6C3CbfDE343");

/// The ERC-7786 hub on Outbe, which delivers the messages of the target chains. CREATE3 proxy
/// under salt "ERC7786Bridgev8.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const ERC7786_HUB_ADDRESS: Address = address!("0x972af8B0E409d745d8F5ca2F7531586128a6408F");
#[cfg(feature = "e2e-test")]
pub const ERC7786_HUB_ADDRESS: Address = address!("0x2cFdf7b8b56300d97F90f51Dc489dbc77dC15555");

/// IntexAuctionTargetRouter, the same address on every target chain and on Outbe. CREATE3 proxy
/// under salt "outbe-intex:IntexAuctionTargetRouter:v8.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const INTEX_AUCTION_TARGET_ROUTER_ADDRESS: Address =
    address!("0x1d7036CAef871a787b4163898791C1eccC693F3a");
#[cfg(feature = "e2e-test")]
pub const INTEX_AUCTION_TARGET_ROUTER_ADDRESS: Address =
    address!("0x7420664797Fe83229224B617aEC0Ba2924F2465e");

/// WCOEN on Outbe. CREATE3 proxy under salt "WCOENv8.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const WCOEN_ADDRESS: Address = address!("0x9dC5964751984d913e19A5c5E4cCF17B1dC75013");
#[cfg(feature = "e2e-test")]
pub const WCOEN_ADDRESS: Address = address!("0x5C8E0466cCD1f66A85f167Fa3ec75E5F48C7145F");

/// The WCOEN token bridge, the same address on every chain. CREATE3 proxy under salt
/// "WCOENBridgev8.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const WCOEN_BRIDGE_ADDRESS: Address = address!("0x819eAaF036b4265E65074130BDa93ac0F8C8Fd88");
#[cfg(feature = "e2e-test")]
pub const WCOEN_BRIDGE_ADDRESS: Address = address!("0x189b63A6583Ffd67082D15456F0fACD0CBb2B631");
