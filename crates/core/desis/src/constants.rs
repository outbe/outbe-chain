use alloy_primitives::{address, Address};

/// OriginRouter on Outbe (outbound ERC-7786 sends).
/// CREATE3 proxy via outbe-intex Create3Factory, salt "outbe-intex:OriginRouter:v3.0.0".
#[cfg(not(feature = "e2e-test"))]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0xCBfa290DCd34319Ff1aec79A4084f2C900977599");

/// Same proxy under salt "outbe-intex:OriginRouter:e2e-test", deployed by the well-known
/// anvil account so a throwaway chain needs no production key.
#[cfg(feature = "e2e-test")]
pub const ORIGIN_ROUTER_ADDRESS: Address = address!("0x6Dda31E7211c31dB8E5AF24c780Cb34526d8411E");

/// Minimum-bid-quantity floor: 4% of the prior series' issued count (basis points).
pub const BID_QUANTITY_FLOOR_BPS: u32 = 400;

/// Currency one Intex strikes in; mirrors the oracle's `DAY_TYPE_ISO`.
pub const PROMIS_LOAD_STRIKE_ISO: u16 = 840;

/// What one Intex strikes at. The ladder steps by decades, so the strike drifts
/// from here up to ten times it before resetting; only powers of ten are valid.
pub const PROMIS_LOAD_STRIKE_USD: u32 = 100;

/// Deadband at each decade boundary, so a rate loitering there stops flipping the load daily.
pub const PROMIS_LOAD_DEADBAND_BPS: u32 = 200;

/// Fixed load bypassing the ladder: an e2e day cannot afford one laddered Intex.
#[cfg(not(feature = "e2e-test"))]
pub const PROMIS_LOAD_OVERRIDE: Option<u128> = None;
#[cfg(feature = "e2e-test")]
pub const PROMIS_LOAD_OVERRIDE: Option<u128> = Some(1);

/// Bid fan-in deadline: clearing proceeds without chains that have not reported
/// BIDS_DONE within this window after the clearing stage starts. A repair window
/// for parked legs; must stay under 24h so the deadline clear lands the same UTC
/// day as the dispatch.
pub const BIDS_FANIN_TIMEOUT_SECS: u64 = 12 * 3600;

/// Midnight-anchored schedule: the commit, reveal and settlement windows each span one day.
#[cfg(not(feature = "e2e-test"))]
pub const COMMIT_WINDOW_SECONDS: u64 = 24 * 3600;
#[cfg(not(feature = "e2e-test"))]
pub const REVEAL_WINDOW_SECONDS: u32 = 24 * 3600;
#[cfg(not(feature = "e2e-test"))]
pub const SETTLEMENT_WINDOW_SECONDS: u64 = 24 * 3600;

/// An e2e run walks the same stages inside one day, so its windows have to fit
/// there: a day-long window would carry the auction over a midnight the run
/// never formed.
#[cfg(feature = "e2e-test")]
pub const COMMIT_WINDOW_SECONDS: u64 = 900;
#[cfg(feature = "e2e-test")]
pub const REVEAL_WINDOW_SECONDS: u32 = 1800;
#[cfg(feature = "e2e-test")]
pub const SETTLEMENT_WINDOW_SECONDS: u64 = 1800;

/// Guarantee at least this much commit window; a brief that would leave less
/// anchors to the next midnight instead.
#[cfg(not(feature = "e2e-test"))]
pub const MIN_COMMIT_WINDOW_SECONDS: u64 = 18 * 3600;
#[cfg(feature = "e2e-test")]
pub const MIN_COMMIT_WINDOW_SECONDS: u64 = 300;

/// `dayState` wire values carried by AUCTION_STAGE_START.
pub const DAY_STATE_GREEN: u8 = 1;
pub const DAY_STATE_RED: u8 = 2;

/// Bidders per REFUND_INSTRUCTIONS message: the codec's `MAX_PAYLOAD_ARRAY_LEN`. Issuance uses its own,
/// narrower cap — a recipient costs a mint, a bidder costs a lock update.
pub use outbe_intexfactory::constants::MAX_RECIPIENTS_PER_MESSAGE as REFUND_CHUNK_LEN;

/// Chunks one chain-day's refunds may span; mirrors the codec's `MAX_CHUNKS`.
pub const MAX_REFUND_CHUNKS: usize = 256;

/// Reference currencies one day may price. Mirrors the codec's `MAX_REFERENCE_PRICES`:
/// a day over it could not be started on any chain.
pub const MAX_REFERENCE_PRICES: usize = 6;

/// `IDesis.InboundIgnored` reason codes; mirrors the routers' `InboundReason` library.
pub const IGNORED_OBSOLETE: u8 = 2;
pub const IGNORED_CONFLICT: u8 = 3;
pub const IGNORED_NOT_FOUND: u8 = 4;
