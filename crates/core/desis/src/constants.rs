use alloy_primitives::{address, Address};

/// OriginRouter on Outbe (outbound ERC-7786 sends).
/// CREATE3 proxy via outbe-intex Create3Factory, salt "outbe-intex:OriginRouter:v3.0.0".
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0xCBfa290DCd34319Ff1aec79A4084f2C900977599");

/// Minimum-bid-quantity floor: 4% of the prior series' issued count (basis points).
pub const BID_QUANTITY_FLOOR_BPS: u32 = 400;

/// PROMIS load: 100k PROMIS per 1 Intex (scale 1e6 PROMIS-units on the wire).
pub const PROMIS_LOAD: u128 = 100_000;

/// Bid fan-in deadline: clearing proceeds without chains that have not reported
/// BIDS_DONE within this window after the clearing stage starts. A repair window
/// for parked legs; must stay under 24h so the deadline clear lands the same UTC
/// day as the dispatch.
pub const BIDS_FANIN_TIMEOUT_SECS: u64 = 12 * 3600;

/// Midnight-anchored schedule: the commit, reveal and settlement windows each span one day.
pub const COMMIT_WINDOW_SECONDS: u64 = 24 * 3600;
pub const REVEAL_WINDOW_SECONDS: u32 = 24 * 3600;
pub const SETTLEMENT_WINDOW_SECONDS: u64 = 24 * 3600;

/// Guarantee at least this much commit window; a brief that would leave less
/// anchors to the next midnight instead.
pub const MIN_COMMIT_WINDOW_SECONDS: u64 = 18 * 3600;

/// `dayState` wire values carried by AUCTION_STAGE_START.
pub const DAY_STATE_GREEN: u8 = 1;
pub const DAY_STATE_RED: u8 = 2;

/// Bidders per REFUND_INSTRUCTIONS message: the same per-message array cap the issuance
/// fan-out uses, and the codec's `MAX_PAYLOAD_ARRAY_LEN`.
pub use outbe_intexfactory::constants::MAX_RECIPIENTS_PER_MESSAGE as REFUND_CHUNK_LEN;

/// Chunks one chain-day's refunds may span; mirrors the codec's `MAX_CHUNKS`.
pub const MAX_REFUND_CHUNKS: usize = 256;

/// Reference currencies one day may price. Mirrors the codec's `MAX_REFERENCE_PRICES`:
/// a day over it could not be started on any chain.
pub const MAX_REFERENCE_PRICES: usize = 6;
