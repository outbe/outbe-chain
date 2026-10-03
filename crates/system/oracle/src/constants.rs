//! Module-local protocol constants.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::math::reference_price::is_coen_iso_market;
use outbe_primitives::units::{SCALE_1E18, SCALE_1E6_U256};

/// Genesis seed for the USD (ISO 840) currency rate: the current SOFR
/// (Secured Overnight Financing Rate) at scale `1e6`.
pub const DEFAULT_USD_CURRENCY_RATE: U256 = U256::from_limbs([36_300u64, 0, 0, 0]);

/// Maximum age of a live COEN/ISO rate used by economic transaction paths and
/// qualification hooks. The raw Oracle query ABI intentionally remains historical.
pub const FX_RATE_MAX_AGE_SECONDS: u64 = 6 * 60 * 60;

/// Raw snapshots older than this are no longer readable.
pub(crate) const MAX_SNAPSHOT_RETENTION_SECONDS: u64 = 365 * 24 * 3600;

/// Width of one hourly VWAP aggregate cell.
pub(crate) const VWAP_HOUR_SECONDS: u64 = 60 * 60;

/// Hourly cells kept per pair; a cell is reused for the same hour a day later.
pub(crate) const HOURLY_VWAP_CELLS: u64 = 24;

/// Minimum share of possible tally rounds that must have produced a snapshot
/// for a pair before its finalized window VWAP is usable: two thirds. The
/// possible rounds are the window's blocks divided by `vote_period`.
pub(crate) const MIN_WINDOW_COVERAGE: (u64, u64) = (2, 3);

/// Maximum number of closed UTC days the begin-block lifecycle finalizes in a
/// single block. Normal operation finalizes exactly one day per UTC-midnight
/// rollover; this cap only bounds catch-up after a long gap (cold start or
/// extended downtime). Days older than the cap stay unfinalized.
pub const MAX_UTC_DAY_VWAP_BACKFILL_DAYS: u32 = 366;

/// ISO 4217 code the day-type pair is quoted in.
pub const DAY_TYPE_ISO: u16 = 840;

/// The day-type pair: COEN quoted in ISO 840. COEN is the zero address, so this
/// is also its sorted storage-key form.
///
/// Spelled as a literal because `AddressPair::new_coen_to` is not const -
/// `copy_from_slice` is not. The
/// `the_day_type_pair_key_is_the_coen_iso_840_pair` test is what keeps it
/// honest.
pub const DAY_TYPE_PAIR: AddressPair = AddressPair::new([
    // COEN - 20 zero bytes.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    // ISO 840 - the marker plus BCD 840.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0c, 0xc8, 0x40,
]);

/// Largest whole price one vote may quote. The stored rate is this many units
/// times the pair scale, so one million on COEN/ISO is raw `1e12`.
pub(crate) const MAX_VOTE_PRICE_WHOLE: u64 = 1_000_000;

/// Largest whole volume one vote may quote. The stored volume is this many
/// units times the pair scale, so one trillion on COEN/ISO is raw `1e18`.
pub(crate) const MAX_VOTE_VOLUME_WHOLE: u64 = 1_000_000_000_000;

/// Price scale used only when taking a reciprocal. Generic Oracle markets keep
/// their existing decimal18 reciprocal contract.
pub(crate) fn reciprocal_scale(pair: AddressPair) -> U256 {
    if is_coen_iso_market(pair) {
        SCALE_1E6_U256
    } else {
        SCALE_1E18
    }
}

/// Weight for an observation whose reported volume is genuinely zero. The
/// weight does not replace the stored volume.
pub(crate) fn zero_volume_weight(pair: AddressPair) -> U256 {
    if is_coen_iso_market(pair) {
        SCALE_1E6_U256
    } else {
        SCALE_1E18
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, U256};
    use outbe_primitives::asset_type::AssetType;

    use super::{reciprocal_scale, zero_volume_weight, AddressPair, DAY_TYPE_ISO, DAY_TYPE_PAIR};

    #[test]
    fn the_day_type_pair_key_is_the_coen_iso_840_pair() {
        assert_eq!(DAY_TYPE_PAIR, AddressPair::new_coen_to(DAY_TYPE_ISO));
    }

    #[test]
    fn every_coen_iso_market_uses_six_decimal_reciprocal_and_zero_volume_scales() {
        for iso in [840, 978] {
            let pair = AddressPair::new_coen_to(iso);
            assert_eq!(reciprocal_scale(pair), U256::from(1_000_000u64));
            assert_eq!(zero_volume_weight(pair), U256::from(1_000_000u64));
        }
    }

    #[test]
    fn non_iso_generic_markets_keep_their_existing_scale() {
        let token = address!("0x1111111111111111111111111111111111111111");
        for pair in [
            AddressPair::from_assets(AssetType::Native, AssetType::ERC20(token)),
            AddressPair::from_assets(AssetType::ERC20(token), AssetType::IsoCurrency(840)),
        ] {
            assert_eq!(
                reciprocal_scale(pair),
                U256::from(1_000_000_000_000_000_000u128)
            );
            assert_eq!(
                zero_volume_weight(pair),
                U256::from(1_000_000_000_000_000_000u128)
            );
        }
    }
}
