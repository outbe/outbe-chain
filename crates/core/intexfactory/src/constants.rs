//! Module-local constants: external contract addresses called via
//! `storage.call`, plus protocol constants.
//!
//! NFT / router addresses are permanent CREATE3 proxies via the outbe-intex
//! Create3Factory (deployer 0x2Af7d3C5C3f82Fee4eA037A674f55fa2eD011c05, salt
//! "outbe-intex:<Name>:v2.0.0") — stable across chains and redeploys.

use alloy_primitives::{address, Address};
use outbe_primitives::time::SECONDS_PER_DAY;

/// Every protocol period below is `u32` seconds.
const DAY: u32 = SECONDS_PER_DAY as u32;

/// IntexNFT1155 on Outbe (balance ledger: settle / burnSettled / balanceOf).
/// CREATE3 proxy, salt "outbe-intex:IntexNFT1155:v2.0.0".
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x4Ccbc413a5f159Da316178F8b7576C923b4D1e5d");

/// OriginRouter on Outbe (outbound ERC-7786 sends).
/// CREATE3 proxy, salt "outbe-intex:OriginRouter:v2.0.0".
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x67129C422bDC2c8984DbF381B6ec4515fE2BbD29");

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
pub const QUALIFICATION_PERIOD: u32 = 21 * DAY;

/// Reference-currency ISO for the qualifier oracle pair (COEN/0xUSD = 840).
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

/// Issuance-currency ISO; fixed to USD (840) until multi-currency lands.
pub const QUALIFIER_ISSUANCE_ISO: u16 = 840;

/// Bin step (basis points) for the floor-price bin ladder.
pub const BIN_STEP_BP: u16 = 25;

/// Markup rates in percentage points: price = entry * (PRICE_RATE_DEN + rate) / PRICE_RATE_DEN.
pub const PRICE_RATE_DEN: u16 = 100;
/// Floor price = entry * 1.08.
pub const FLOOR_RATE: u16 = 8;
/// Call price = entry * 2.28; its breach arms a Call Event.
pub const CALL_RATE: u16 = 128;

/// Notice a holder gets to settle after a series is Called.
pub const CALL_NOTICE_PERIOD: u32 = 7 * DAY;

/// Call-trigger evaluation window: the most recent stretch scanned for breaches.
pub const CALL_WINDOW: u32 = 28 * DAY;
/// Call-trigger threshold: how much of the window must be in breach to force-call.
pub const CALL_THRESHOLD: u32 = 21 * DAY;

/// Commit-entry bond on the target-chain auction: 100M wCOEN (18-dec minor units).
pub const COMMIT_BOND_MINOR: u128 = 100_000_000 * 10u128.pow(18);
