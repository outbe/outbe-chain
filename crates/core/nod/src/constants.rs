use outbe_primitives::time::SECONDS_PER_DAY_U32;

/// ERC-721 metadata surface values.
pub const TOKEN_NAME: &str = "Nod";
pub const TOKEN_SYMBOL: &str = "NOD";
pub const TOKEN_DESCRIPTION: &str = concat!(
    "Nod is a mining right on the Outbe network. Its owner pays the settlement cost at the fixed ",
    "entry price, and the settled right is exercised to mine its Gratis load. ",
    "A called Nod must be settled by its settlement deadline, otherwise it is forfeited. ",
    "A Nod is never transferable."
);

/// The first issuance of a group snapshots the four call terms below onto it,
/// and every later check reads the group's copy. Retuning one
/// of them re-terms groups issued afterwards. It leaves every already-issued
/// group on the terms it was issued with. Gem and intex give the same guarantee.
///
/// Call-price markup percent: `call = entry x (100 + CALL_RATE_PCT) / 100`
/// (256 => +256%, i.e. 3.56x entry). Same shape as credis' 64 and
/// gem/intex's 128, one rung up the same ladder.
pub const CALL_RATE_PCT: u16 = 256;

/// Trailing window the daily call scan inspects, in whole UTC days.
pub const CALL_LOOKBACK_DAYS: u32 = 28;

/// Qualifying days within the lookback window that arm a call. A day at or
/// below the call price, and a day with no published price, both simply fail
/// to count, so the window absorbs up to
/// `CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS` of either.
pub const CALL_THRESHOLD_DAYS: u32 = 21;

/// [`CALL_LOOKBACK_DAYS`] in seconds - the encoding `group_call_window_seconds`
/// seals at issuance, matching `GemData::call_window_seconds`.
pub const CALL_WINDOW: u32 = CALL_LOOKBACK_DAYS * SECONDS_PER_DAY_U32;

/// [`CALL_THRESHOLD_DAYS`] in seconds - the encoding
/// `group_call_threshold_seconds` seals at issuance, matching
/// `GemData::call_threshold_seconds`.
pub const CALL_THRESHOLD: u32 = CALL_THRESHOLD_DAYS * SECONDS_PER_DAY_U32;

/// Seconds after `called_at` within which the owner must settle. Once the
/// period elapses, the CycleTick forfeit sweep forfeits the group's unpaid Nods and
/// the cleanup deletes them.
pub const CALL_NOTICE_PERIOD: u32 = 7 * SECONDS_PER_DAY_U32;
