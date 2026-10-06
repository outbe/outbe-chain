//! Module-local protocol constants for credis positions.

pub const TOKEN_NAME: &str = "Credis";
pub const TOKEN_SYMBOL: &str = "CREDIS";
pub const TOKEN_DESCRIPTION: &str = concat!(
    "Credis is a position on the Outbe network, owned by a smart account and backed by pledged ",
    "Gratis. Its owner repays it directly in the original asset, interest first, and each repayment ",
    "returns pledged Gratis in proportion to the principal it covers. A called Credis must be ",
    "settled by its settlement deadline, otherwise it is forfeited."
);

/// Denominator for [`CALL_RATE_PCT`].
pub const PRICE_RATE_DEN: u16 = 100;

/// The call price adds 64% to `call_anchor_price_minor`. A sustained breach arms the call.
pub const CALL_RATE_PCT: u16 = 64;

/// Seconds in a day, for the second-encoded call terms below. A position seals
/// its window and threshold in seconds, the way gem's record does. The daily
/// scan divides them back into day counts.
pub const SECS_PER_DAY: u32 = 24 * 60 * 60;

/// Evaluation window for the call, in closed UTC days: the daily scan looks
/// back this far over the official daily reference series. Distinct from
/// [`CALL_NOTICE_PERIOD`], which is the settlement window the call itself opens.
pub const CALL_LOOKBACK_DAYS: u32 = 28;

/// Qualifying days within the lookback window that arm a call. A day at or
/// below the call price does not count. A day with no published price also
/// does not count. Thus the window absorbs up to
/// `CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS` of either. Mirrors gem's
/// `CALL_WINDOW` / `CALL_THRESHOLD` pair.
pub const CALL_THRESHOLD_DAYS: u32 = 21;

/// [`CALL_LOOKBACK_DAYS`] in seconds. This is the encoding that
/// `Position::call_window_seconds` seals at opening, matching
/// `GemData::call_window_seconds`.
pub const CALL_WINDOW: u32 = CALL_LOOKBACK_DAYS * SECS_PER_DAY;

/// [`CALL_THRESHOLD_DAYS`] in seconds. This is the encoding that
/// `Position::call_threshold_seconds` seals at opening, matching
/// `GemData::call_threshold_seconds`.
pub const CALL_THRESHOLD: u32 = CALL_THRESHOLD_DAYS * SECS_PER_DAY;

/// Settlement window that the call opens, in seconds. The name describes what it
/// is and the `Position::call_notice_period_seconds` field it seals. It does not
/// name the window that it is not.
pub const CALL_NOTICE_PERIOD: u32 = 7 * SECS_PER_DAY;

/// Day count convention for interest accrual: simple, ACT/365.
pub const DAYS_PER_YEAR: u64 = 365;

/// Basis-point multiplier applied to the currency's official policy rate when
/// pinning a position's `policy_rate` at opening. 10_000 bp = x1.
// TODO: confirm the policy-rate factor before launch. Section 10 lists it as TBD and proposes 1.
// A governance-settable parameter is the upgrade path if it needs retuning
// without a redeploy.
pub const POLICY_RATE_FACTOR_BP: u32 = 10_000;

/// Denominator for [`POLICY_RATE_FACTOR_BP`].
pub const BP_DEN: u32 = 10_000;
