//! Module-local constants: external contract addresses called via
//! `storage.call`, plus protocol constants.
//!
//! NFT / router addresses are permanent CREATE3 proxies via the outbe-intex
//! Create3Factory (deployer 0x2Af7d3C5C3f82Fee4eA037A674f55fa2eD011c05, salt
//! "outbe-intex:<Name>:v2.0.0") — stable across chains and redeploys.

use alloy_primitives::{address, Address};

/// IntexNFT1155 on Outbe (balance ledger: settle / burnSettled / balanceOf).
/// CREATE3 proxy, salt "outbe-intex:IntexNFT1155:v2.0.0".
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x4Ccbc413a5f159Da316178F8b7576C923b4D1e5d");

/// OriginRouter on Outbe (outbound ERC-7786 sends).
/// CREATE3 proxy, salt "outbe-intex:OriginRouter:v2.0.0".
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x67129C422bDC2c8984DbF381B6ec4515fE2BbD29");

/// minePromis PoW difficulty: required leading zero bytes of the work hash.
pub const POW_DIFFICULTY: usize = 1;

/// Qualification maturity in days since issuance (mirrors Nod's MATURITY_PERIOD_DAYS).
pub const MATURITY_PERIOD_DAYS: u64 = 21;
/// Derived seconds, for comparison against block timestamps.
pub const MATURITY_PERIOD_SECONDS: u64 = MATURITY_PERIOD_DAYS * 24 * 60 * 60;

/// Reference-currency ISO for the qualifier oracle pair (COEN/0xUSD = 840).
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

/// Bin step (basis points) for the floor-price bin ladder.
pub const BIN_STEP_BP: u16 = 25;

/// Floor price = COEN/0xUSD price * 1.08; integer ratio 108/100.
pub const FLOOR_PRICE_NUM: u64 = 108;
pub const FLOOR_PRICE_DEN: u64 = 100;

/// Call price = COEN/0xUSD price * 2.28; integer ratio 228/100.
pub const CALL_PRICE_NUM: u64 = 228;
pub const CALL_PRICE_DEN: u64 = 100;

/// Forced-settlement deadline after a series is Called, in seconds.
pub const INTEX_CALL_PERIOD_SECONDS: u32 = 21 * 24 * 3600;

/// Call-trigger evaluation window: most recent completed days scanned for breaches.
pub const CALL_WINDOW_DAYS: u16 = 30;
/// Call-trigger threshold: breach-days within the window required to force-call.
pub const CALL_THRESHOLD_DAYS: u16 = 21;
