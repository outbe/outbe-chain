/// ERC-721 metadata surface values.
pub const TOKEN_NAME: &str = "Nod";
pub const TOKEN_SYMBOL: &str = "NOD";
pub const TOKEN_DESCRIPTION: &str = concat!(
    "Nod is a mining right on the Outbe network. Once qualified, its owner pays the settlement cost ",
    "at the fixed entry price, and the settled right is exercised to mine its Gratis load. ",
    "A called Nod must be settled by its settlement deadline, otherwise it is forfeited. ",
    "A Nod is never transferable."
);

/// The first issuance of a bucket snapshots the four call terms below onto it,
/// and every later check reads the bucket's copy. Retuning one
/// of them re-terms buckets issued afterwards. It leaves every already-issued
/// bucket on the terms it was issued with. Gem and intex give the same guarantee.
///
/// Call-price markup percent: `call = entry x (100 + CALL_RATE_PCT) / 100`
/// (256 => +256%, i.e. 3.56x entry). Same shape as credis' 64 and
/// gem/intex's 128, one rung up the same ladder.
pub const CALL_RATE_PCT: u16 = 256;

/// Floor markup percent: `floor = entry x (100 + FLOOR_RATE_PCT) / 100`, the rate gem and intex use.
pub const FLOOR_RATE_PCT: u16 = 8;

/// Seconds in a day. The call terms a bucket seals are second-encoded, the way
/// gem's record stores them. The daily scan divides them back into day counts.
pub const SECS_PER_DAY: u32 = 24 * 3600;

/// Trailing window the daily call scan inspects, in whole UTC days.
pub const CALL_LOOKBACK_DAYS: u32 = 28;

/// Qualifying days within the lookback window that arm a call. A day at or
/// below the call price, and a day with no published price, both simply fail
/// to count, so the window absorbs up to
/// `CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS` of either.
pub const CALL_THRESHOLD_DAYS: u32 = 21;

/// [`CALL_LOOKBACK_DAYS`] in seconds - the encoding `callable_bucket_call_window_seconds`
/// seals at issuance, matching `GemData::call_window_seconds`.
pub const CALL_WINDOW: u32 = CALL_LOOKBACK_DAYS * SECS_PER_DAY;

/// [`CALL_THRESHOLD_DAYS`] in seconds - the encoding
/// `callable_bucket_call_threshold_seconds` seals at issuance, matching
/// `GemData::call_threshold_seconds`.
pub const CALL_THRESHOLD: u32 = CALL_THRESHOLD_DAYS * SECS_PER_DAY;

/// Seconds after `called_at` within which the owner must settle. Once the
/// period elapses, the CycleTick forfeit sweep burns the bucket's remaining Nods.
pub const CALL_NOTICE_PERIOD: u32 = 7 * SECS_PER_DAY;

/// Buckets visited per call slice. The cursors resume the rest on the next
/// CycleTick against the same frozen UTC day.
pub const MAX_NOD_CALL_VISITS_PER_BLOCK: u32 = 4096;

/// Forfeit-queue steps per block: one per Nod burned or queue slot visited. The
/// cap is far below the call visit budget because a forfeit is a compressed-entity
/// load plus delete rather than an EVM slot write.
/// A correlated mass-forfeit is the expected shape of a call event, not a tail
/// case, so the burst needs its own cap.
pub const MAX_NOD_FORFEITS_PER_BLOCK: u32 = 256;

/// `SweepDaySkipped.sweep` of the Called sweep.
pub const CALL_SWEEP: u8 = 1;
