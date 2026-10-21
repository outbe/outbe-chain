pub use outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS;

/// Minimum-bid-quantity floor: 4% of the prior series' issued count (basis points).
pub const BID_QUANTITY_FLOOR_BPS: u32 = 400;

/// The rung the first Intex carries: 10^12 minor, 1 000 000 PROMIS at six decimals.
pub const PROMIS_LOAD_LAUNCH_EXPONENT: u32 = 12;

/// Fixed load bypassing the ladder. The harness seeds the day's mint cap at 500
/// PROMIS, far under one laddered Intex, so a laddered e2e day would issue none.
#[cfg(not(feature = "e2e-test"))]
pub const PROMIS_LOAD_OVERRIDE: Option<u128> = None;
#[cfg(feature = "e2e-test")]
pub const PROMIS_LOAD_OVERRIDE: Option<u128> = Some(1);

/// Bid fan-in deadline: clearing proceeds without chains that have not reported
/// BIDS_DONE within this window after the clearing stage starts. A repair window
/// for parked legs. It must stay under 24h so the deadline clear lands on the
/// same UTC day as the dispatch.
pub const BIDS_FANIN_TIMEOUT_SECS: u64 = 12 * 3600;

/// Midnight-anchored schedule: the commit, reveal and settlement windows each span one day.
#[cfg(not(feature = "e2e-test"))]
pub const COMMIT_WINDOW_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;
#[cfg(not(feature = "e2e-test"))]
pub const REVEAL_WINDOW_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;
#[cfg(not(feature = "e2e-test"))]
pub const SETTLEMENT_WINDOW_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;

/// An e2e run walks the same stages inside one day, so its windows have to fit
/// there: a day-long window would carry the auction over a midnight the run
/// never formed.
#[cfg(feature = "e2e-test")]
pub const COMMIT_WINDOW_SECONDS: u64 = 900;
#[cfg(feature = "e2e-test")]
pub const REVEAL_WINDOW_SECONDS: u64 = 1800;
#[cfg(feature = "e2e-test")]
pub const SETTLEMENT_WINDOW_SECONDS: u64 = 1800;

/// An auction that would start with less commit window than this is cancelled.
#[cfg(not(feature = "e2e-test"))]
pub const MIN_COMMIT_WINDOW_SECONDS: u64 = 16 * 3600;
#[cfg(feature = "e2e-test")]
pub const MIN_COMMIT_WINDOW_SECONDS: u64 = 300;

/// `dayState` wire values carried by AUCTION_STAGE_START.
pub const DAY_STATE_LIVE: u8 = 1;
pub const DAY_STATE_CANCELLED: u8 = 2;

/// Entries one cross-chain payload array carries: bids per PROCESS_BIDS_BATCH and winners per
/// REFUND_INSTRUCTIONS; clearing sizes its gas in chunks of the same length. Mirrors
/// `BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN`.
pub const MAX_PAYLOAD_ARRAY_LEN: usize = 64;

/// Chunks one chain-day's refunds may span; mirrors the codec's `MAX_CHUNKS`.
pub const MAX_REFUND_CHUNKS: usize = 256;

/// Batches one chain-day may send: the arrival bitmap is one 256-bit word.
pub const MAX_BID_BATCHES: u16 = 256;

// Everything the intake admits from one chain must still fit its refund fan-out: a bid batch
// and a refund chunk share one array cap, so the batch count bounds the chunk count.
const _: () = assert!(MAX_BID_BATCHES as usize <= MAX_REFUND_CHUNKS);

/// `IDesis.InboundIgnored` reason codes; mirrors the routers' `InboundReason` library.
pub const IGNORED_OBSOLETE: u8 = 2;
pub const IGNORED_CONFLICT: u8 = 3;
pub const IGNORED_NOT_FOUND: u8 = 4;

/// What a CLEARING round costs on a target chain - fixed part, one chunk, one bid - measured in
/// `contracts/intex/test/foundry/cross-chain/ClearingRelayMailboxGas.t.sol`.
pub const CLEARING_FIXED_GAS: u64 = 564_000;
pub const CLEARING_CHUNK_GAS: u64 = 197_000;
pub const CLEARING_BID_GAS: u64 = 10_000;

/// Days of a chain's own history a round is sized from, and the floor it is never sized below.
pub const CLEARING_HISTORY_DAYS: u32 = 7;
pub const CLEARING_MIN_BIDS: u64 = 32;
