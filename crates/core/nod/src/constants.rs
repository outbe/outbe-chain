/// Legacy ERC-721 metadata surface values.
pub const TOKEN_NAME: &str = "Nod";
pub const TOKEN_SYMBOL: &str = "NOD";
pub const TOKEN_DESCRIPTION: &str = "Outbe Nod";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/nod/image/";

/// Per-bin multiplicative step in basis points. PancakeSwap LB default; each
/// bin spans a 0.25% price band. The LB-protocol constants used alongside
/// this value (`SCALE`, `SCALE_OFFSET`, `PRECISION`, `BASIS_POINT_MAX`,
/// `REAL_ID_SHIFT`, `MAX_BIN_ID`) live in `outbe_primitives::math::constants`.
pub const BIN_STEP_BP: u16 = 25;

/// The four call terms below are snapshotted onto a bucket when it is first
/// issued, and every later check reads the bucket's copy. Retuning one of them
/// re-terms buckets issued afterwards, and leaves every already-issued bucket
/// on the terms it was issued with - the same guarantee gem and intex give.
///
/// Call-price markup percent: `call = entry x (100 + CALL_RATE_PCT) / 100`
/// (256 => +256%, i.e. 3.56x entry). Same shape as credis' 64 and
/// gem/intex's 128, one rung up the same ladder.
pub const CALL_RATE_PCT: u16 = 256;

/// Seconds in a day. The call terms a bucket seals are second-encoded, the way
/// gem's record stores them; the daily scan divides them back into day counts.
pub const SECS_PER_DAY: u32 = 24 * 3600;

/// Trailing window the daily call scan inspects, in whole UTC days.
pub const CALL_LOOKBACK_DAYS: u32 = 28;

/// Qualifying days within the lookback window that arm a call. A day at or
/// below the call price, and a day with no published price, both simply fail
/// to count, so the window absorbs up to
/// `CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS` of either.
pub const CALL_THRESHOLD_DAYS: u32 = 21;

/// [`CALL_LOOKBACK_DAYS`] in seconds - the encoding `callable_bucket_call_window`
/// seals at issuance, matching `GemData::call_window`.
pub const CALL_WINDOW: u32 = CALL_LOOKBACK_DAYS * SECS_PER_DAY;

/// [`CALL_THRESHOLD_DAYS`] in seconds - the encoding
/// `callable_bucket_call_threshold` seals at issuance, matching
/// `GemData::call_threshold`.
pub const CALL_THRESHOLD: u32 = CALL_THRESHOLD_DAYS * SECS_PER_DAY;

/// Seconds after `called_at` within which the owner must settle. Once
/// elapsed the bucket's remaining Nods are forfeit-burned.
pub const CALL_NOTICE_PERIOD: u32 = 7 * SECS_PER_DAY;

/// Buckets visited per call slice, across the call and forfeit arms; the cursors
/// resume the rest on the next CycleTick against the same frozen UTC day.
pub const MAX_NOD_CALL_VISITS_PER_BLOCK: u32 = 4096;

/// Nod bodies forfeit-burned per call slice, far below the visit budget because a
/// forfeit is a compressed-entity load plus delete rather than an EVM slot write.
/// A correlated mass-forfeit is the expected shape of a call event, not a tail
/// case, so the burst needs its own cap.
pub const MAX_NOD_FORFEITS_PER_BLOCK: u32 = 256;

/// `SweepDaySkipped.sweep` of the Called sweep; 0 belonged to the retired qualify sweep.
pub const CALL_SWEEP: u8 = 1;
