//! Module-local constants: external contract addresses called via
//! `storage.call`, plus protocol constants.
//!
//! NFT / messenger addresses are permanent CREATE3 proxies via the outbe-intex
//! Create3Factory (deployer 0x2Af7d3C5C3f82Fee4eA037A674f55fa2eD011c05, salt
//! "outbe-intex:<Name>:v1.0.0") — stable across chains and redeploys.

use alloy_primitives::{address, Address};

/// Outbe reserve VaultProvider (settlement deposits).
pub const RESERVE_VAULT: Address = address!("0xC8ce1EFE882B0fbb1E2ABBEed828316bb282b76d");

/// IntexNFT1155 on Outbe (balance ledger: settle / burnSettled / balanceOf).
/// CREATE3 proxy, salt "outbe-intex:IntexNFT1155:v1.0.0".
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x6f9335086f166c94e4d272a07ac2DA848a7BCE83");

/// OriginMessenger on Outbe (outbound LayerZero sends).
/// CREATE3 proxy, salt "outbe-intex:OriginMessenger:v1.0.0".
pub const ORIGIN_MESSENGER_ADDRESS: Address =
    address!("0x53c5DB9AAf0Ecf8A8c734b2d6C88fE2e56F2f955");

/// minePromis PoW difficulty: required leading zero bytes of the work hash.
pub const POW_DIFFICULTY: usize = 1;

/// Qualification maturity: strictly more than 21 days since issuance.
pub const MATURITY_PERIOD_SECONDS: u64 = 21 * 24 * 60 * 60;

/// Reference-currency ISO for the qualifier oracle pair (COEN/0xUSD = 840).
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

/// Bin step (basis points) for the floor-price bin ladder.
pub const BIN_STEP_BP: u16 = 25;

/// COEN price floor = clearing price * 1.08; integer ratio 108/100.
pub const COEN_PRICE_FLOOR_NUM: u64 = 108;
pub const COEN_PRICE_FLOOR_DEN: u64 = 100;

/// Forced-call trigger = floor * 1.64; integer ratio 164/100.
pub const COEN_CALL_TRIGGER_NUM: u64 = 164;
pub const COEN_CALL_TRIGGER_DEN: u64 = 100;
