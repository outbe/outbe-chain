//! Module-local constants: external contract addresses called via
//! `storage.call`, plus protocol constants.
//!
//! NFT / router addresses are permanent CREATE3 proxies via the outbe-intex
//! Create3Factory (deployer 0x2Af7d3C5C3f82Fee4eA037A674f55fa2eD011c05, salt
//! "outbe-intex:<Name>:v2.0.0") — stable across chains and redeploys.

use alloy_primitives::{address, Address};

/// IntexNFT1155 on Outbe (balance ledger: settle / burnSettled / balanceOf).
/// CREATE3 proxy, salt "outbe-intex:IntexNFT1155:v2.0.0". Canonical definition
/// lives in `outbe_primitives::addresses`; re-exported here for existing callers.
#[cfg(not(feature = "e2e-test"))]
pub use outbe_primitives::addresses::INTEX_NFT1155_ADDRESS;

/// Same proxy under salt "outbe-intex:IntexNFT1155:e2e-test", deployed by the well-known
/// anvil account so a throwaway chain needs no production key.
#[cfg(feature = "e2e-test")]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x671cB5f90CA40A601F248615204699A826dbFd0d");

/// OriginRouter on Outbe (outbound ERC-7786 sends).
/// CREATE3 proxy, salt "outbe-intex:OriginRouter:v2.0.0".
#[cfg(not(feature = "e2e-test"))]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x67129C422bDC2c8984DbF381B6ec4515fE2BbD29");

/// Same proxy under salt "outbe-intex:OriginRouter:e2e-test".
#[cfg(feature = "e2e-test")]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x6Dda31E7211c31dB8E5AF24c780Cb34526d8411E");

/// minePromis PoW difficulty: required leading zero bytes of the work hash.
pub const POW_DIFFICULTY: usize = 1;

/// Max contributor payouts per `distribute` pass (pagination chunk size).
/// Large series are drained across several blocks by the begin-block hook.
pub const DIST_CHUNK_LIMIT: u32 = 200;

/// Proceeds fan-in window: creators are paid once every winning chain has
/// routed its proceeds, or this long after issuance — whichever comes first.
/// A full day absorbs legitimate escrow-finalize retries so, in virtually all
/// cases, creators receive a single payment.
pub const PROCEEDS_FANIN_TIMEOUT_SECS: u64 = 24 * 60 * 60;

/// Time a series must age past `issued_at` before it can become Qualified.
pub const QUALIFICATION_PERIOD: u32 = 21 * 24 * 3600;

/// Bin step (basis points) for the floor-price bin ladder.
pub const BIN_STEP_BP: u16 = 25;

/// Markup rates in percentage points: price = entry * (PRICE_RATE_DEN + rate) / PRICE_RATE_DEN.
pub const PRICE_RATE_DEN: u16 = 100;

/// Floor price = entry * 1.08.
pub const FLOOR_RATE: u16 = 8;
/// Call price = entry * 2.28; its breach arms a Call Event.
pub const CALL_RATE: u16 = 128;

/// Notice a holder gets to settle after a series is Called.
pub const CALL_NOTICE_PERIOD: u32 = 7 * 24 * 3600;

/// Call-trigger evaluation window: the most recent stretch scanned for breaches.
pub const CALL_WINDOW: u32 = 28 * 24 * 3600;
/// Call-trigger threshold: how much of the window must be in breach to force-call.
pub const CALL_THRESHOLD: u32 = 21 * 24 * 3600;

/// Oracle prices carry 1e18, the wire and the target chains 1e9. Moves with
/// `PRICE_DECIMALS = 9` in `IntexMetadata`.
pub const ORACLE_TO_WIRE_SCALE: u64 = 1_000_000_000;

/// Commit-entry bond on the target-chain auction: 100M wCOEN (18-dec minor units).
pub const COMMIT_BOND_MINOR: u128 = 100_000_000 * 10u128.pow(18);

/// How old a COEN rate may be and still convert a settlement into the issuance currency.
/// Seconds, not vote periods: those are counted in blocks and stretch under congestion.
pub const FX_RATE_MAX_AGE_SECONDS: u64 = 6 * 3600;

/// Series one ISSUANCE_INSTRUCTIONS message may carry, and recipients across them.
/// Mirror the codec's `MAX_SERIES_PER_ISSUANCE` and `MAX_PAYLOAD_ARRAY_LEN`.
pub const MAX_SERIES_PER_MESSAGE: usize = 8;
pub const MAX_RECIPIENTS_PER_MESSAGE: usize = 64;
