//! External CREATE3 contract addresses owned by `core` domains.

use alloy_primitives::{address, Address};

/// IntexNFT1155: the Intex ERC-1155 series balance ledger, not a precompile. CREATE3 proxy under salt
/// "outbe-intex:IntexNFT1155:v7.0.0", deployer 0x2Af7d3C5…1c05, factory 0xe78edad9…d14b.
#[cfg(not(feature = "e2e-test"))]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x8E5041c14D3f0b802E8eFA4e575E5234265C79a6");
#[cfg(feature = "e2e-test")]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x116063CF0558225D808d1C309322d6C3CbfDE343");

/// The ERC-7786 hub on Outbe, which delivers the messages of the target chains. CREATE3 proxy
/// under salt "ERC7786Bridgev7.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const ERC7786_HUB_ADDRESS: Address = address!("0x1925D1af9dEd78172EF305Ff46A5999d05505f32");
#[cfg(feature = "e2e-test")]
pub const ERC7786_HUB_ADDRESS: Address = address!("0x2cFdf7b8b56300d97F90f51Dc489dbc77dC15555");

/// IntexAuctionTargetRouter, the same address on every target chain and on Outbe. CREATE3 proxy
/// under salt "outbe-intex:IntexAuctionTargetRouter:v7.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const INTEX_AUCTION_TARGET_ROUTER_ADDRESS: Address =
    address!("0x27a9DABEe8ea2d624E85d0034d92bD1daf0D54B3");
#[cfg(feature = "e2e-test")]
pub const INTEX_AUCTION_TARGET_ROUTER_ADDRESS: Address =
    address!("0x7420664797Fe83229224B617aEC0Ba2924F2465e");

/// WCOEN on Outbe. CREATE3 proxy under salt "WCOENv7.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const WCOEN_ADDRESS: Address = address!("0x3414f55f086Eb224135D9044299E6eBc6E841197");
#[cfg(feature = "e2e-test")]
pub const WCOEN_ADDRESS: Address = address!("0x5C8E0466cCD1f66A85f167Fa3ec75E5F48C7145F");

/// The WCOEN token bridge, the same address on every chain. CREATE3 proxy under salt
/// "WCOENBridgev7.0.0", same deployer and factory.
#[cfg(not(feature = "e2e-test"))]
pub const WCOEN_BRIDGE_ADDRESS: Address = address!("0xDEb9bA2f46395D03c6ff51b4A2c4898930ED170b");
#[cfg(feature = "e2e-test")]
pub const WCOEN_BRIDGE_ADDRESS: Address = address!("0x189b63A6583Ffd67082D15456F0fACD0CBb2B631");
