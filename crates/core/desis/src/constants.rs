pub use outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS;

/// Minimum-bid-quantity floor: 4% of the prior series' issued count (basis points).
pub const BID_QUANTITY_FLOOR_BPS: u32 = 400;

/// Fixed load bypassing the ladder. The harness seeds the day's mint cap at 500
/// PROMIS, far under one laddered Intex, so a laddered e2e day would issue none.
#[cfg(not(feature = "e2e-test"))]
pub const PROMIS_LOAD_OVERRIDE: Option<u128> = None;
#[cfg(feature = "e2e-test")]
pub const PROMIS_LOAD_OVERRIDE: Option<u128> = Some(1);

/// Bid fan-in deadline: clearing proceeds without chains that have not reported
/// BIDS_DONE within this window after the collection stage starts. A repair window
/// for parked legs. It must stay under 24h so the deadline clear lands on the
/// same UTC day as the dispatch.
pub const BIDS_FANIN_TIMEOUT_SECS: u64 = 8 * 3600;

/// Mirrors `OriginRouter.MAX_TARGETS`. The work budgets of Desis are measured with this many
/// targets, so a longer snapshot fails the day instead of the run.
pub const MAX_TARGET_CHAINS: usize = 16;

/// Trigger budget that the `auction_advance` sweep keeps for one more day. The sweep stops
/// before a day when less than this remains.
///
/// The value is the measured gross maximum of one day × 3, rounded up to 1,000 gas. The
/// maximum is 847,229: the reveal end of a day with 16 chains, which arms the clearing
/// (`calibration_four_due_auctions_at_midnight` in `outbe-evm`). The factor is 3, not 4, so
/// that the budget of `auction_advance` (four due days × 4) also pays for the fourth day.
pub const ADVANCE_DAY_RESERVE_GAS: u64 = 2_542_000;

/// Midnight-anchored schedule: the commit, reveal and Intex issuance windows each span one day.
#[cfg(not(feature = "e2e-test"))]
pub const COMMIT_WINDOW_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;
#[cfg(not(feature = "e2e-test"))]
pub const REVEAL_WINDOW_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;
#[cfg(not(feature = "e2e-test"))]
pub const INTEX_ISSUANCE_WINDOW_SECONDS: u64 = outbe_primitives::time::SECONDS_PER_DAY;

/// An e2e run walks the same stages inside one day, so its windows have to fit
/// there: a day-long window would carry the auction over a midnight the run
/// never formed.
#[cfg(feature = "e2e-test")]
pub const COMMIT_WINDOW_SECONDS: u64 = 900;
#[cfg(feature = "e2e-test")]
pub const REVEAL_WINDOW_SECONDS: u64 = 1800;
#[cfg(feature = "e2e-test")]
pub const INTEX_ISSUANCE_WINDOW_SECONDS: u64 = 1800;

/// Desis clears no later than this before the end of the Intex issuance window, so a result
/// sent at the last moment still reaches every chain before the day ends there.
#[cfg(not(feature = "e2e-test"))]
pub const RESULT_DELIVERY_MARGIN_SECONDS: u64 = 8 * 3600;
#[cfg(feature = "e2e-test")]
pub const RESULT_DELIVERY_MARGIN_SECONDS: u64 = 600;

/// Mirrors `EscrowTimes.UNFINALIZED_REFUND_DELAY`: the other chains read a day without a result
/// as failed once this much has passed since its commit end.
pub const TARGET_UNFINALIZED_REFUND_DELAY_SECONDS: u64 = 48 * 3600;

// A result sent at the last clearing time must still find the day awaiting it on the other chains.
const _: () = assert!(
    REVEAL_WINDOW_SECONDS + INTEX_ISSUANCE_WINDOW_SECONDS
        <= TARGET_UNFINALIZED_REFUND_DELAY_SECONDS
);

/// An auction that would start with less commit window than this is cancelled.
#[cfg(not(feature = "e2e-test"))]
pub const MIN_COMMIT_WINDOW_SECONDS: u64 = 16 * 3600;
#[cfg(feature = "e2e-test")]
pub const MIN_COMMIT_WINDOW_SECONDS: u64 = 300;

/// `dayState` wire values carried by AUCTION_STAGE_START.
pub const DAY_STATE_LIVE: u8 = 1;
pub const DAY_STATE_CANCELLED: u8 = 2;

/// `IDesis.IntexAuctionCanceled` reason codes. Priority at start is this order.
pub const CANCELED_NO_LIMIT: u8 = 1;
pub const CANCELED_UNPRICED: u8 = 2;
pub const CANCELED_BELOW_ONE_UNIT: u8 = 3;
pub const CANCELED_LATE_START: u8 = 4;
pub const CANCELED_LIMIT_OVERFLOW: u8 = 5;

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

/// `IDesis.BidsIgnored` reason codes; mirrors the routers' `InboundReason` library.
pub const IGNORED_OBSOLETE: u8 = 2;
pub const IGNORED_CONFLICT: u8 = 3;
pub const IGNORED_NOT_FOUND: u8 = 4;

/// What a COLLECTING_BIDS round costs on a target chain - fixed part, one chunk, one bid - measured in
/// `contracts/intex/test/foundry/cross-chain/ClearingRelayMailboxGas.t.sol`.
pub const CLEARING_FIXED_GAS: u64 = 564_000;
pub const CLEARING_CHUNK_GAS: u64 = 197_000;
pub const CLEARING_BID_GAS: u64 = 10_000;

/// Days of a chain's own history a round is sized from, and the floor it is never sized below.
pub const CLEARING_HISTORY_DAYS: u32 = 7;
pub const CLEARING_MIN_BIDS: u64 = 32;

/// Cleanup work per Cycle body, independent of auction trigger budgets.
pub const CLEANUP_WORK_UNITS: u32 = 256;
/// Bounds empty chains and empty days as well as populated chain headers.
pub const CLEANUP_MAX_HEADERS: u32 = 4;
/// Eight 10k-gas units cover at most nine metadata writes plus header reads.
pub const CLEANUP_HEADER_UNITS: u32 = 8;
