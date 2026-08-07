//! Module-local protocol constants.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;

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

/// ISO 4217 code the day-type pair is quoted in.
pub const DAY_TYPE_ISO: u16 = 840;

/// [`DAY_TYPE_PAIR`] as a storage key: COEN (the zero address, which sorts
/// first) quoted in ISO 840.
///
/// Spelled as a literal because `AddressPair::from_addresses` sorts, and
/// neither `Ord` nor `copy_from_slice` is const. The
/// `the_day_type_pair_key_is_the_coen_iso_840_pair` test is what keeps it
/// honest.
pub const DAY_TYPE_PAIR_KEY: AddressPair = AddressPair::new([
    // COEN — 20 zero bytes.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    // ISO 840 — the marker plus BCD 840.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0c, 0xc8, 0x40,
]);

#[cfg(test)]
mod tests {
    use super::{DAY_TYPE_ISO, DAY_TYPE_PAIR_KEY};
    use outbe_primitives::asset_type::coen_iso_pair;

    #[test]
    fn the_day_type_pair_key_is_the_coen_iso_840_pair() {
        assert_eq!(DAY_TYPE_PAIR_KEY, coen_iso_pair(DAY_TYPE_ISO));
    }
}
