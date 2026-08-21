//! Module-local constants: external contract addresses called via
//! `storage.call`, plus protocol constants.
//!
//! NFT / router addresses are permanent CREATE3 proxies via the outbe-intex
//! Create3Factory (deployer 0x2Af7d3C5C3f82Fee4eA037A674f55fa2eD011c05, salt
//! "outbe-intex:<Name>:v3.0.0") — stable across chains and redeploys.

use alloy_primitives::{address, Address};
use outbe_primitives::units::SCALE_1E6_U128;

/// IntexNFT1155 on Outbe (balance ledger: settle / burnSettled / balanceOf).
/// CREATE3 proxy, salt "outbe-intex:IntexNFT1155:v3.0.0". Canonical definition
/// lives in `outbe_primitives::addresses`; re-exported here for existing callers.
#[cfg(not(feature = "e2e-test"))]
pub use outbe_primitives::addresses::INTEX_NFT1155_ADDRESS;

/// Same proxy under salt "outbe-intex:IntexNFT1155:e2e-test", deployed by the
/// well-known anvil account so a throwaway chain needs no production key.
#[cfg(feature = "e2e-test")]
pub const INTEX_NFT1155_ADDRESS: Address = address!("0x671cB5f90CA40A601F248615204699A826dbFd0d");

/// OriginRouter on Outbe (outbound ERC-7786 sends).
/// CREATE3 proxy, salt "outbe-intex:OriginRouter:v3.0.0".
#[cfg(not(feature = "e2e-test"))]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0xCBfa290DCd34319Ff1aec79A4084f2C900977599");

/// Same proxy under salt "outbe-intex:OriginRouter:e2e-test".
#[cfg(feature = "e2e-test")]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x6Dda31E7211c31dB8E5AF24c780Cb34526d8411E");

/// A payout e2e credits proceeds without deploying the router, so a throwaway
/// build also accepts Hardhat account #0 as their source.
#[cfg(feature = "e2e-test")]
pub const PROCEEDS_TEST_SENDER: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");

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

/// Work one lifecycle scan may do: a decision reads a group, an action writes one
/// series with its index move and notice. Budgeted apart because they differ in cost.
pub(crate) const MAX_GROUP_DECISIONS_PER_SWEEP: u32 = 256;
pub(crate) const MAX_SERIES_ACTIONS_PER_SWEEP: u32 = 256;

/// Queue entries drained per `intex_notify` firing. One entry can still fan out to several messages,
/// so this bounds the entries taken from the queue, not the sends they produce.
pub const NOTIFY_CHUNK_LIMIT: u32 = 32;

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

/// Commit-entry bond on the target-chain auction: 100M in wire units, which the
/// adapter's six-decimal pin makes equal to the payment token's minor units.
pub const COMMIT_BOND_MINOR: u128 = 100_000_000 * SCALE_1E6_U128;

/// How old a COEN rate may be and still convert a settlement into the issuance currency.
/// Seconds, not vote periods: those are counted in blocks and stretch under congestion.
pub const FX_RATE_MAX_AGE_SECONDS: u64 = 6 * 3600;

/// Series one ISSUANCE_INSTRUCTIONS message may carry. Mirrors `MAX_SERIES_PER_ISSUANCE`.
pub const MAX_SERIES_PER_MESSAGE: usize = 8;

/// Recipients one ISSUANCE_INSTRUCTIONS may carry across its series. Mirrors the codec's
/// `MAX_RECIPIENTS_PER_ISSUANCE`, which is narrower than the general payload cap because a
/// recipient costs a mint on the destination.
pub const MAX_RECIPIENTS_PER_ISSUANCE: usize = 32;

/// The general cross-chain array cap, mirroring `MAX_PAYLOAD_ARRAY_LEN`. Refund chunks use it.
pub const MAX_RECIPIENTS_PER_MESSAGE: usize = 64;

/// Series one MARK_CALLED or MARK_QUALIFIED message may carry. Mirrors the
/// codec's `MAX_SERIES_PER_MARK`; a wider group is sent in several messages.
pub const MAX_SERIES_PER_MARK: usize = 8;
