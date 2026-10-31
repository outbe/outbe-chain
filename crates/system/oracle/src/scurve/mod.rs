//! S-curve pricing algorithm.
//!
//! The curve shape comes from Cosmos SDK `x/oracle/algorithm/scurve.go`. The
//! stored coefficients are quantized to the COEN/840 six-decimal contract.
//! Every 128-day segment applies the same coefficient table. The next segment
//! starts from `floor(previous_anchor * 0.92)`, forming one continuous chain.

mod query;
mod store;

use alloy_primitives::U256;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::units::SCALE_1E6_U256;

pub use query::{
    get_all_scurve_data, get_all_scurve_data_for_pair, get_scurve_entries, scurve_value,
};
pub(crate) use store::process_wwd_scurves;
pub use store::{store_scurve_entry, SCURVE_RING_SLOTS};

/// S-curve period in days.
pub const PERIOD: usize = 128;

/// Truncates a timestamp to the start of the UTC day (00:00 UTC).
pub fn truncate_to_day(timestamp: u64) -> u64 {
    (timestamp / SECONDS_PER_DAY) * SECONDS_PER_DAY
}

/// Precomputed COEN/840 S-curve coefficients at six-decimal scale (128 entries).
///
/// Formula: `coef_i = 1 - alpha * (sigmoid(x_i) - sigmoid_min) / (sigmoid_0 - sigmoid_min)`
/// Parameters: alpha=0.08, beta=0.08, center=64
///
/// Source: `x/oracle/algorithm/scurve.go:precomputedCoefficients`
///
/// Each value is `floor(original_decimal18 / 10^12)`.
pub static COEFFICIENTS: [U256; PERIOD] = [
    U256::from_limbs([1_000_000, 0, 0, 0]), // [0] 1.000000000000000000
    U256::from_limbs([999_960, 0, 0, 0]),   // [1]
    U256::from_limbs([999_917, 0, 0, 0]),   // [2]
    U256::from_limbs([999_870, 0, 0, 0]),   // [3]
    U256::from_limbs([999_820, 0, 0, 0]),   // [4]
    U256::from_limbs([999_765, 0, 0, 0]),   // [5]
    U256::from_limbs([999_706, 0, 0, 0]),   // [6]
    U256::from_limbs([999_642, 0, 0, 0]),   // [7]
    U256::from_limbs([999_573, 0, 0, 0]),   // [8]
    U256::from_limbs([999_498, 0, 0, 0]),   // [9]
    U256::from_limbs([999_418, 0, 0, 0]),   // [10]
    U256::from_limbs([999_330, 0, 0, 0]),   // [11]
    U256::from_limbs([999_236, 0, 0, 0]),   // [12]
    U256::from_limbs([999_134, 0, 0, 0]),   // [13]
    U256::from_limbs([999_024, 0, 0, 0]),   // [14]
    U256::from_limbs([998_905, 0, 0, 0]),   // [15]
    U256::from_limbs([998_776, 0, 0, 0]),   // [16]
    U256::from_limbs([998_638, 0, 0, 0]),   // [17]
    U256::from_limbs([998_488, 0, 0, 0]),   // [18]
    U256::from_limbs([998_326, 0, 0, 0]),   // [19]
    U256::from_limbs([998_152, 0, 0, 0]),   // [20]
    U256::from_limbs([997_964, 0, 0, 0]),   // [21]
    U256::from_limbs([997_762, 0, 0, 0]),   // [22]
    U256::from_limbs([997_543, 0, 0, 0]),   // [23]
    U256::from_limbs([997_308, 0, 0, 0]),   // [24]
    U256::from_limbs([997_055, 0, 0, 0]),   // [25]
    U256::from_limbs([996_783, 0, 0, 0]),   // [26]
    U256::from_limbs([996_490, 0, 0, 0]),   // [27]
    U256::from_limbs([996_175, 0, 0, 0]),   // [28]
    U256::from_limbs([995_837, 0, 0, 0]),   // [29]
    U256::from_limbs([995_474, 0, 0, 0]),   // [30]
    U256::from_limbs([995_085, 0, 0, 0]),   // [31]
    U256::from_limbs([994_668, 0, 0, 0]),   // [32]
    U256::from_limbs([994_221, 0, 0, 0]),   // [33]
    U256::from_limbs([993_744, 0, 0, 0]),   // [34]
    U256::from_limbs([993_233, 0, 0, 0]),   // [35]
    U256::from_limbs([992_687, 0, 0, 0]),   // [36]
    U256::from_limbs([992_105, 0, 0, 0]),   // [37]
    U256::from_limbs([991_485, 0, 0, 0]),   // [38]
    U256::from_limbs([990_825, 0, 0, 0]),   // [39]
    U256::from_limbs([990_124, 0, 0, 0]),   // [40]
    U256::from_limbs([989_379, 0, 0, 0]),   // [41]
    U256::from_limbs([988_590, 0, 0, 0]),   // [42]
    U256::from_limbs([987_756, 0, 0, 0]),   // [43]
    U256::from_limbs([986_874, 0, 0, 0]),   // [44]
    U256::from_limbs([985_944, 0, 0, 0]),   // [45]
    U256::from_limbs([984_965, 0, 0, 0]),   // [46]
    U256::from_limbs([983_937, 0, 0, 0]),   // [47]
    U256::from_limbs([982_859, 0, 0, 0]),   // [48]
    U256::from_limbs([981_731, 0, 0, 0]),   // [49]
    U256::from_limbs([980_553, 0, 0, 0]),   // [50]
    U256::from_limbs([979_327, 0, 0, 0]),   // [51]
    U256::from_limbs([978_053, 0, 0, 0]),   // [52]
    U256::from_limbs([976_733, 0, 0, 0]),   // [53]
    U256::from_limbs([975_368, 0, 0, 0]),   // [54]
    U256::from_limbs([973_961, 0, 0, 0]),   // [55]
    U256::from_limbs([972_515, 0, 0, 0]),   // [56]
    U256::from_limbs([971_033, 0, 0, 0]),   // [57]
    U256::from_limbs([969_517, 0, 0, 0]),   // [58]
    U256::from_limbs([967_974, 0, 0, 0]),   // [59]
    U256::from_limbs([966_405, 0, 0, 0]),   // [60]
    U256::from_limbs([964_817, 0, 0, 0]),   // [61]
    U256::from_limbs([963_213, 0, 0, 0]),   // [62]
    U256::from_limbs([961_599, 0, 0, 0]),   // [63]
    U256::from_limbs([959_980, 0, 0, 0]),   // [64] center
    U256::from_limbs([958_360, 0, 0, 0]),   // [65]
    U256::from_limbs([956_746, 0, 0, 0]),   // [66]
    U256::from_limbs([955_143, 0, 0, 0]),   // [67]
    U256::from_limbs([953_554, 0, 0, 0]),   // [68]
    U256::from_limbs([951_986, 0, 0, 0]),   // [69]
    U256::from_limbs([950_442, 0, 0, 0]),   // [70]
    U256::from_limbs([948_927, 0, 0, 0]),   // [71]
    U256::from_limbs([947_444, 0, 0, 0]),   // [72]
    U256::from_limbs([945_998, 0, 0, 0]),   // [73]
    U256::from_limbs([944_591, 0, 0, 0]),   // [74]
    U256::from_limbs([943_227, 0, 0, 0]),   // [75]
    U256::from_limbs([941_906, 0, 0, 0]),   // [76]
    U256::from_limbs([940_632, 0, 0, 0]),   // [77]
    U256::from_limbs([939_406, 0, 0, 0]),   // [78]
    U256::from_limbs([938_228, 0, 0, 0]),   // [79]
    U256::from_limbs([937_101, 0, 0, 0]),   // [80]
    U256::from_limbs([936_022, 0, 0, 0]),   // [81]
    U256::from_limbs([934_994, 0, 0, 0]),   // [82]
    U256::from_limbs([934_015, 0, 0, 0]),   // [83]
    U256::from_limbs([933_085, 0, 0, 0]),   // [84]
    U256::from_limbs([932_204, 0, 0, 0]),   // [85]
    U256::from_limbs([931_369, 0, 0, 0]),   // [86]
    U256::from_limbs([930_580, 0, 0, 0]),   // [87]
    U256::from_limbs([929_836, 0, 0, 0]),   // [88]
    U256::from_limbs([929_134, 0, 0, 0]),   // [89]
    U256::from_limbs([928_474, 0, 0, 0]),   // [90]
    U256::from_limbs([927_854, 0, 0, 0]),   // [91]
    U256::from_limbs([927_272, 0, 0, 0]),   // [92]
    U256::from_limbs([926_727, 0, 0, 0]),   // [93]
    U256::from_limbs([926_216, 0, 0, 0]),   // [94]
    U256::from_limbs([925_738, 0, 0, 0]),   // [95]
    U256::from_limbs([925_291, 0, 0, 0]),   // [96]
    U256::from_limbs([924_874, 0, 0, 0]),   // [97]
    U256::from_limbs([924_485, 0, 0, 0]),   // [98]
    U256::from_limbs([924_122, 0, 0, 0]),   // [99]
    U256::from_limbs([923_784, 0, 0, 0]),   // [100]
    U256::from_limbs([923_469, 0, 0, 0]),   // [101]
    U256::from_limbs([923_176, 0, 0, 0]),   // [102]
    U256::from_limbs([922_904, 0, 0, 0]),   // [103]
    U256::from_limbs([922_651, 0, 0, 0]),   // [104]
    U256::from_limbs([922_416, 0, 0, 0]),   // [105]
    U256::from_limbs([922_198, 0, 0, 0]),   // [106]
    U256::from_limbs([921_995, 0, 0, 0]),   // [107]
    U256::from_limbs([921_807, 0, 0, 0]),   // [108]
    U256::from_limbs([921_633, 0, 0, 0]),   // [109]
    U256::from_limbs([921_471, 0, 0, 0]),   // [110]
    U256::from_limbs([921_322, 0, 0, 0]),   // [111]
    U256::from_limbs([921_183, 0, 0, 0]),   // [112]
    U256::from_limbs([921_054, 0, 0, 0]),   // [113]
    U256::from_limbs([920_935, 0, 0, 0]),   // [114]
    U256::from_limbs([920_825, 0, 0, 0]),   // [115]
    U256::from_limbs([920_723, 0, 0, 0]),   // [116]
    U256::from_limbs([920_629, 0, 0, 0]),   // [117]
    U256::from_limbs([920_542, 0, 0, 0]),   // [118]
    U256::from_limbs([920_461, 0, 0, 0]),   // [119]
    U256::from_limbs([920_386, 0, 0, 0]),   // [120]
    U256::from_limbs([920_317, 0, 0, 0]),   // [121]
    U256::from_limbs([920_253, 0, 0, 0]),   // [122]
    U256::from_limbs([920_194, 0, 0, 0]),   // [123]
    U256::from_limbs([920_140, 0, 0, 0]),   // [124]
    U256::from_limbs([920_089, 0, 0, 0]),   // [125]
    U256::from_limbs([920_043, 0, 0, 0]),   // [126]
    U256::from_limbs([920_000, 0, 0, 0]),   // [127]
];

/// Multiplies a value by a scale-6 factor without overflowing `U256`.
fn mul_scale_1e6(value: U256, factor: U256) -> U256 {
    let whole = value / SCALE_1E6_U256;
    let remainder = value % SCALE_1E6_U256;
    whole * factor + remainder * factor / SCALE_1E6_U256
}

/// Computes the continuous S-curve value for a peak and absolute day index.
///
/// Each 128-day period starts from the previous period's anchor multiplied by
/// exactly `0.92` with floor rounding. The coefficient table is then applied
/// at the offset inside that period. This preserves the day-127/day-128
/// boundary and continues the same chain indefinitely.
pub fn compute_scurve_value(peak_price: U256, day_index: usize) -> U256 {
    let periods = day_index / PERIOD;
    let offset = day_index % PERIOD;
    let successor_factor = COEFFICIENTS[PERIOD - 1];
    let mut anchor = peak_price;
    for _ in 0..periods {
        anchor = mul_scale_1e6(anchor, successor_factor);
        if anchor.is_zero() {
            break;
        }
    }
    mul_scale_1e6(anchor, COEFFICIENTS[offset])
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use outbe_primitives::address_pair::AddressPair;
    use outbe_primitives::units::SCALE_1E6_U64 as COEN840_SCALE;

    use outbe_primitives::time::WorldwideDay;

    use super::*;
    use crate::schema::OracleContract;

    fn price6(whole: u64) -> U256 {
        U256::from(whole) * U256::from(COEN840_SCALE)
    }

    #[test]
    fn test_coefficients_boundary_values() {
        // COEN/840 coefficients use the same six-decimal denominator as prices.
        assert_eq!(COEFFICIENTS[0], U256::from(1_000_000u64));
        // Last coefficient is exactly 0.92.
        assert_eq!(COEFFICIENTS[127], U256::from(920_000u64));
        // Center (64) should be ~0.95998
        assert_eq!(COEFFICIENTS[64], U256::from(959_980u64));
    }

    #[test]
    fn coen840_coefficients_and_products_match_reference_vector() {
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../../testdata/coen840-scurve-v1.json")).unwrap();
        assert_eq!(vector["pair"], "COEN/840");
        assert_eq!(vector["coefficientScale"], COEN840_SCALE.to_string());
        assert_eq!(vector["rounding"], "floor");

        let coefficients = vector["coefficients"].as_array().unwrap();
        assert_eq!(coefficients.len(), PERIOD);
        for (index, expected) in coefficients.iter().enumerate() {
            let expected = expected.as_str().unwrap().parse::<u64>().unwrap();
            assert_eq!(
                COEFFICIENTS[index],
                U256::from(expected),
                "coefficient[{index}]"
            );
        }

        for pin in vector["productPins"].as_array().unwrap() {
            let index = pin["index"].as_u64().unwrap() as usize;
            let peak = pin["peakPrice"].as_str().unwrap().parse::<u64>().unwrap();
            let expected = pin["expected"].as_str().unwrap().parse::<u64>().unwrap();
            assert_eq!(
                compute_scurve_value(U256::from(peak), index),
                U256::from(expected),
                "product pin {index}"
            );
        }
    }

    #[test]
    fn test_coefficients_monotonically_decreasing() {
        for i in 1..PERIOD {
            assert!(
                COEFFICIENTS[i] <= COEFFICIENTS[i - 1],
                "coefficient[{}] > coefficient[{}]: {} > {}",
                i,
                i - 1,
                COEFFICIENTS[i],
                COEFFICIENTS[i - 1]
            );
        }
    }

    fn scale_floor(value: U256, factor: U256) -> U256 {
        let whole = value / SCALE_1E6_U256;
        let remainder = value % SCALE_1E6_U256;
        whole * factor + remainder * factor / SCALE_1E6_U256
    }

    fn period_anchor(mut peak: U256, periods: usize) -> U256 {
        for _ in 0..periods {
            peak = scale_floor(peak, COEFFICIENTS[PERIOD - 1]);
        }
        peak
    }

    fn expected_curve_value(peak: U256, day: usize) -> U256 {
        let anchor = period_anchor(peak, day / PERIOD);
        scale_floor(anchor, COEFFICIENTS[day % PERIOD])
    }

    #[test]
    fn curve_chains_across_periods() {
        let peak_price = price6(100);
        for day in [0, 127, 128, 255, 256, 383, 384, 1_000] {
            assert_eq!(
                compute_scurve_value(peak_price, day),
                expected_curve_value(peak_price, day),
                "day {day}"
            );
        }
        assert_eq!(
            compute_scurve_value(peak_price, 127),
            compute_scurve_value(peak_price, 128)
        );
        assert_eq!(
            compute_scurve_value(peak_price, 255),
            compute_scurve_value(peak_price, 256)
        );
        assert!(!compute_scurve_value(peak_price, 1_000).is_zero());
    }

    #[test]
    fn curve_math_does_not_overflow_to_zero() {
        let peak = U256::MAX;
        for day in [0, 1, 127, 128, 256, 1_000] {
            let value = compute_scurve_value(peak, day);
            assert_eq!(value, expected_curve_value(peak, day), "day {day}");
            assert!(!value.is_zero(), "day {day}");
            assert!(value <= peak, "day {day}");
        }
    }

    #[test]
    fn curve_preserves_fractional_precision() {
        // Test with a non-round peak price
        let peak = U256::from(18_343_660_000u64); // 18343.66 on the COEN/840 scale
        let val_day1 = compute_scurve_value(peak, 1);
        // Expected: 18343.66 * 0.999960180425626732 = ~18342.93 (approx)
        // The exact value depends on integer truncation
        assert!(val_day1 < peak);
        assert!(val_day1 > price6(18_342));
    }

    #[test]
    fn test_truncate_to_day() {
        // 2025-01-01 12:34:56 UTC
        let ts = 1735735696u64;
        let day = truncate_to_day(ts);
        assert_eq!(day % SECONDS_PER_DAY, 0);
        assert!(day <= ts);
        assert!(ts - day < SECONDS_PER_DAY);
    }

    /// WWD `day` days after 2024-01-01.
    fn wwd(day: u64) -> WorldwideDay {
        WorldwideDay::from_timestamp(
            outbe_primitives::time::date_key_to_utc_timestamp(20_240_101).unwrap()
                + day * SECONDS_PER_DAY,
        )
    }

    fn with_pair(test: impl FnOnce(&mut OracleContract, AddressPair)) {
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            let pair = register_test_pair_with_iso(&mut oracle, 840);
            test(&mut oracle, pair);
        });
    }

    fn register_test_pair_with_iso(oracle: &mut OracleContract, iso_code: u16) -> AddressPair {
        let quote: Address = crate::types::AssetType::IsoCurrency(iso_code).into();
        let pair = AddressPair::from_addresses(Address::ZERO, quote);
        oracle.register_pair(pair).unwrap();
        pair
    }

    #[test]
    fn a_curve_continues_after_day_128() {
        with_pair(|oracle, pair| {
            assert!(store_scurve_entry(oracle, pair, wwd(0), price6(500)).unwrap());

            assert_eq!(scurve_value(oracle, pair, wwd(0)).unwrap(), price6(500));
            assert_eq!(scurve_value(oracle, pair, wwd(127)).unwrap(), price6(460));
            assert_eq!(scurve_value(oracle, pair, wwd(128)).unwrap(), price6(460));
            assert_eq!(
                get_scurve_entries(oracle, pair, wwd(128)).unwrap(),
                (vec![wwd(0).into()], vec![price6(500)], vec![price6(460)])
            );
        });
    }

    #[test]
    fn a_price_at_or_below_the_newest_curve_starts_no_curve() {
        with_pair(|oracle, pair| {
            store_scurve_entry(oracle, pair, wwd(0), price6(100)).unwrap();

            assert!(!store_scurve_entry(oracle, pair, wwd(10), price6(90)).unwrap());
            assert!(!store_scurve_entry(oracle, pair, wwd(0), price6(200)).unwrap());
            // At day 128 the curve is at 92, so 93 starts a curve below the old peak.
            assert!(store_scurve_entry(oracle, pair, wwd(128), price6(93)).unwrap());
            assert_eq!(oracle.scurve_len.read(&pair).unwrap(), 2);
        });
    }

    #[test]
    fn a_new_curve_keeps_the_older_one_for_earlier_wwds() {
        with_pair(|oracle, pair| {
            store_scurve_entry(oracle, pair, wwd(0), price6(100)).unwrap();
            store_scurve_entry(oracle, pair, wwd(9), price6(105)).unwrap();

            assert_eq!(
                scurve_value(oracle, pair, wwd(5)).unwrap(),
                compute_scurve_value(price6(100), 5)
            );
            assert_eq!(scurve_value(oracle, pair, wwd(9)).unwrap(), price6(105));
        });
    }

    #[test]
    fn the_next_curve_overwrites_the_oldest_in_a_full_ring() {
        with_pair(|oracle, pair| {
            for day in 0..=SCURVE_RING_SLOTS {
                store_scurve_entry(oracle, pair, wwd(day), price6(100 + day)).unwrap();
            }

            assert_eq!(scurve_value(oracle, pair, wwd(0)).unwrap(), U256::ZERO);
            assert_eq!(scurve_value(oracle, pair, wwd(1)).unwrap(), price6(101));
            assert_eq!(
                get_all_scurve_data_for_pair(oracle, pair).unwrap().0.len() as u64,
                SCURVE_RING_SLOTS
            );
        });
    }

    #[test]
    fn curves_are_isolated_by_pair() {
        with_pair(|oracle, usd| {
            let eur = register_test_pair_with_iso(oracle, 978);
            store_scurve_entry(oracle, usd, wwd(0), price6(100)).unwrap();
            store_scurve_entry(oracle, eur, wwd(0), price6(200)).unwrap();
            store_scurve_entry(oracle, usd, wwd(1), price6(150)).unwrap();

            assert_eq!(scurve_value(oracle, usd, wwd(1)).unwrap(), price6(150));
            assert_eq!(
                scurve_value(oracle, eur, wwd(1)).unwrap(),
                compute_scurve_value(price6(200), 1)
            );
        });
    }

    #[test]
    fn a_wwd_starts_a_curve_only_when_its_utc_day_has_a_vwap() {
        with_pair(|oracle, usd| {
            let cad = register_test_pair_with_iso(oracle, 124);
            let usd_index = oracle.pair_index_of(usd).unwrap();
            let cad_index = oracle.pair_index_of(cad).unwrap();
            oracle
                .record_utc_day_vwap(wwd(1).value(), usd_index, price6(1))
                .unwrap();
            oracle
                .record_utc_day_vwap(wwd(1).value(), cad_index, price6(1))
                .unwrap();
            let vwaps = [(usd, price6(100)), (cad, price6(100))];

            process_wwd_scurves(oracle, wwd(0), &vwaps).unwrap();
            assert_eq!(oracle.scurve_len.read(&usd).unwrap(), 0);

            process_wwd_scurves(oracle, wwd(1), &vwaps).unwrap();
            assert_eq!(scurve_value(oracle, usd, wwd(1)).unwrap(), price6(100));
            // CAD is not a reference currency.
            assert_eq!(oracle.scurve_len.read(&cad).unwrap(), 0);
        });
    }
}
