//! Module-local protocol constants.

use alloy_primitives::U256;

/// Genesis seed for the USD (ISO 840) currency rate: the current SOFR
/// (Secured Overnight Financing Rate) at 1e18 scale.
pub const DEFAULT_USD_CURRENCY_RATE: U256 = U256::from_limbs([36_300_000_000_000_000u64, 0, 0, 0]);

/// Maximum number of snapshots to retain (approximately 1 year at 2-block vote
/// period with 12-second blocks: ~1.3M snapshots).
pub(crate) const MAX_SNAPSHOT_RETENTION_SECONDS: u64 = 365 * 24 * 3600;

/// Maximum number of closed UTC days the begin-block lifecycle finalizes in a
/// single block. Normal operation finalizes exactly one day per UTC-midnight
/// rollover; this cap only bounds catch-up after a long gap (cold start or
/// extended downtime). Days older than the cap stay unfinalized — their source
/// aggregates are evicted past `MAX_SNAPSHOT_RETENTION_SECONDS` anyway, so they
/// could not be recomputed regardless.
pub const MAX_UTC_DAY_VWAP_BACKFILL_DAYS: u32 = 366;

/// The pair whose WorldwideDay VWAP drives the GREEN/RED day-type decision.
pub const DAY_TYPE_PAIR: (&str, &str) = ("COEN", "840");
