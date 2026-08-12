use alloy_primitives::{address, Address};

/// OriginRouter on Outbe (outbound ERC-7786 sends).
/// CREATE3 proxy via outbe-intex Create3Factory, salt "outbe-intex:OriginRouter:v2.0.0".
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x67129C422bDC2c8984DbF381B6ec4515fE2BbD29");

/// Minimum-bid-quantity floor: 4% of the prior series' issued count (basis points).
pub const BID_QUANTITY_FLOOR_BPS: u32 = 400;

/// Promis load: 100k Promis per 1 Intex (scaled to 18-dec minor on the wire as `promis_load_minor`).
pub const PROMIS_LOAD: u128 = 100_000;

/// Fixed-point scale for bid rates: 1_000_000 = 100% of the escrow basis. Must match the
/// target chain (`BridgeMsgCodec` / `IntexAuction`).
pub const RATE_SCALE: u32 = 1_000_000;

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

/// Bidders per REFUND_INSTRUCTIONS message. Mirrors the codec's per-message array
/// cap; a chain's whole bidder set is relayed as a run of chunks.
pub const REFUND_CHUNK_LEN: usize = 64;

/// Chunks one chain-day's refunds may span. Mirrors the codec bound, which keeps a
/// receiver's arrival set inside one 256-bit word, and matches bid intake's own
/// ceiling of 64 bids across 256 batches.
pub const MAX_REFUND_CHUNKS: usize = 256;
