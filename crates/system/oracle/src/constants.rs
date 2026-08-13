//! Module-local protocol constants.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;

/// Genesis seed for the USD (ISO 840) official annual policy rate — the Fed
/// funds rate — at 1e18 scale (3.63%).
///
/// This is a bootstrap placeholder, not a live figure: the rate a chain runs with
/// comes from its genesis config, and `setCurrencyRate` republishes it whenever
/// the central bank moves. Credis pins whatever is published here on a position
/// at opening and holds it for the position's life.
pub const DEFAULT_USD_POLICY_RATE: U256 = U256::from_limbs([36_300_000_000_000_000u64, 0, 0, 0]);

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

/// ISO 4217 code the day-type pair is quoted in.
pub const DAY_TYPE_ISO: u16 = 840;

/// The day-type pair: COEN quoted in ISO 840. COEN is the zero address, so this
/// is also its sorted storage-key form.
///
/// Spelled as a literal because `AddressPair::new_coen_to` is not const —
/// `copy_from_slice` is not. The
/// `the_day_type_pair_key_is_the_coen_iso_840_pair` test is what keeps it
/// honest.
pub const DAY_TYPE_PAIR: AddressPair = AddressPair::new([
    // COEN — 20 zero bytes.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    // ISO 840 — the marker plus BCD 840.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0c, 0xc8, 0x40,
]);

#[cfg(test)]
mod tests {
    use super::{AddressPair, DAY_TYPE_ISO, DAY_TYPE_PAIR};

    #[test]
    fn the_day_type_pair_key_is_the_coen_iso_840_pair() {
        assert_eq!(DAY_TYPE_PAIR, AddressPair::new_coen_to(DAY_TYPE_ISO));
    }
}
