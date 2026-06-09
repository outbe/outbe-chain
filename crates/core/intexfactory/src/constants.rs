//! Module-local constants: external contract addresses called via
//! `storage.call`, plus protocol constants.
//!
//! The NFT / messenger addresses are current Outbe-testnet deploys (not
//! deterministic); finalized when roles/addresses are re-pointed at the
//! precompile. The module is dormant until then.

use alloy_primitives::{address, Address};

/// Outbe reserve VaultProvider (settlement deposits).
pub const RESERVE_VAULT: Address = address!("0xC8ce1EFE882B0fbb1E2ABBEed828316bb282b76d");

/// IntexNFT1155 on Outbe (balance ledger: settle / burnSettled / balanceOf).
/// CREATE2 via Arachnid, salt 0x496e7465784e4654313135350000000000000000000000000000000000000000 ("IntexNFT1155").
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x1DD9bBeAc4F784145DBFD222aa7Cb16EBBb11631");

/// OriginMessenger on Outbe (outbound LayerZero sends).
/// CREATE2 via Arachnid, salt 0x4f726967696e4d657373656e6765720000000000000000000000000000000000 ("OriginMessenger").
pub const ORIGIN_MESSENGER_ADDRESS: Address =
    address!("0xE679410bD1fFB32238581Aa165749ca0f68Af38d");

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
