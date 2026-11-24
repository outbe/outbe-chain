//! S-curve pricing algorithm.
//!
//! The curve shape comes from Cosmos SDK `x/oracle/algorithm/scurve.go`. The
//! stored coefficients are quantized to the COEN/840 six-decimal contract.
//! Every 128-day segment applies the same coefficient table. The next segment
//! starts from `floor(previous_anchor * 0.84)`, forming one continuous chain.

mod query;
mod store;

use alloy_primitives::U256;
use outbe_system_common::time::SECONDS_PER_DAY;
use outbe_system_common::units::SCALE_1E6_U256;

pub use query::{curve, get_all_scurve_data, scurve_value};
pub(crate) use store::process_hourly_scurves;
pub use store::store_scurve_entry;

/// S-curve period in days.
pub const PERIOD: usize = 128;

/// Truncates a timestamp to the start of the UTC day (00:00 UTC).
pub fn truncate_to_day(timestamp: u64) -> u64 {
    (timestamp / SECONDS_PER_DAY) * SECONDS_PER_DAY
}

/// Precomputed COEN/840 S-curve coefficients at six-decimal scale (128 entries).
///
/// Formula: `coef_i = 1 - alpha * (s(i) - s(0)) / (s(127) - s(0))`, where
/// `s(i) = 1 / (1 + exp(-beta * (i - center)))`.
/// Parameters: alpha=0.16, beta=0.08, center=64
///
/// The Cosmos SDK table in `x/oracle/algorithm/scurve.go` uses alpha=0.08.
/// This table uses the same formula with alpha=0.16.
///
/// Each value is `floor(coef_i * 10^6)`.
pub static COEFFICIENTS: [U256; PERIOD] = [
    U256::from_limbs([1_000_000, 0, 0, 0]), // [0] 1.000000000000000000
    U256::from_limbs([999_920, 0, 0, 0]),   // [1]
    U256::from_limbs([999_834, 0, 0, 0]),   // [2]
    U256::from_limbs([999_740, 0, 0, 0]),   // [3]
    U256::from_limbs([999_640, 0, 0, 0]),   // [4]
    U256::from_limbs([999_530, 0, 0, 0]),   // [5]
    U256::from_limbs([999_412, 0, 0, 0]),   // [6]
    U256::from_limbs([999_285, 0, 0, 0]),   // [7]
    U256::from_limbs([999_146, 0, 0, 0]),   // [8]
    U256::from_limbs([998_997, 0, 0, 0]),   // [9]
    U256::from_limbs([998_836, 0, 0, 0]),   // [10]
    U256::from_limbs([998_661, 0, 0, 0]),   // [11]
    U256::from_limbs([998_472, 0, 0, 0]),   // [12]
    U256::from_limbs([998_268, 0, 0, 0]),   // [13]
    U256::from_limbs([998_048, 0, 0, 0]),   // [14]
    U256::from_limbs([997_810, 0, 0, 0]),   // [15]
    U256::from_limbs([997_553, 0, 0, 0]),   // [16]
    U256::from_limbs([997_276, 0, 0, 0]),   // [17]
    U256::from_limbs([996_976, 0, 0, 0]),   // [18]
    U256::from_limbs([996_653, 0, 0, 0]),   // [19]
    U256::from_limbs([996_305, 0, 0, 0]),   // [20]
    U256::from_limbs([995_929, 0, 0, 0]),   // [21]
    U256::from_limbs([995_524, 0, 0, 0]),   // [22]
    U256::from_limbs([995_087, 0, 0, 0]),   // [23]
    U256::from_limbs([994_617, 0, 0, 0]),   // [24]
    U256::from_limbs([994_111, 0, 0, 0]),   // [25]
    U256::from_limbs([993_566, 0, 0, 0]),   // [26]
    U256::from_limbs([992_981, 0, 0, 0]),   // [27]
    U256::from_limbs([992_351, 0, 0, 0]),   // [28]
    U256::from_limbs([991_675, 0, 0, 0]),   // [29]
    U256::from_limbs([990_949, 0, 0, 0]),   // [30]
    U256::from_limbs([990_171, 0, 0, 0]),   // [31]
    U256::from_limbs([989_337, 0, 0, 0]),   // [32]
    U256::from_limbs([988_443, 0, 0, 0]),   // [33]
    U256::from_limbs([987_488, 0, 0, 0]),   // [34]
    U256::from_limbs([986_466, 0, 0, 0]),   // [35]
    U256::from_limbs([985_375, 0, 0, 0]),   // [36]
    U256::from_limbs([984_211, 0, 0, 0]),   // [37]
    U256::from_limbs([982_970, 0, 0, 0]),   // [38]
    U256::from_limbs([981_650, 0, 0, 0]),   // [39]
    U256::from_limbs([980_248, 0, 0, 0]),   // [40]
    U256::from_limbs([978_759, 0, 0, 0]),   // [41]
    U256::from_limbs([977_181, 0, 0, 0]),   // [42]
    U256::from_limbs([975_512, 0, 0, 0]),   // [43]
    U256::from_limbs([973_748, 0, 0, 0]),   // [44]
    U256::from_limbs([971_888, 0, 0, 0]),   // [45]
    U256::from_limbs([969_931, 0, 0, 0]),   // [46]
    U256::from_limbs([967_874, 0, 0, 0]),   // [47]
    U256::from_limbs([965_718, 0, 0, 0]),   // [48]
    U256::from_limbs([963_462, 0, 0, 0]),   // [49]
    U256::from_limbs([961_107, 0, 0, 0]),   // [50]
    U256::from_limbs([958_654, 0, 0, 0]),   // [51]
    U256::from_limbs([956_106, 0, 0, 0]),   // [52]
    U256::from_limbs([953_466, 0, 0, 0]),   // [53]
    U256::from_limbs([950_736, 0, 0, 0]),   // [54]
    U256::from_limbs([947_923, 0, 0, 0]),   // [55]
    U256::from_limbs([945_030, 0, 0, 0]),   // [56]
    U256::from_limbs([942_066, 0, 0, 0]),   // [57]
    U256::from_limbs([939_035, 0, 0, 0]),   // [58]
    U256::from_limbs([935_948, 0, 0, 0]),   // [59]
    U256::from_limbs([932_811, 0, 0, 0]),   // [60]
    U256::from_limbs([929_634, 0, 0, 0]),   // [61]
    U256::from_limbs([926_426, 0, 0, 0]),   // [62]
    U256::from_limbs([923_198, 0, 0, 0]),   // [63]
    U256::from_limbs([919_960, 0, 0, 0]),   // [64] center
    U256::from_limbs([916_721, 0, 0, 0]),   // [65]
    U256::from_limbs([913_493, 0, 0, 0]),   // [66]
    U256::from_limbs([910_286, 0, 0, 0]),   // [67]
    U256::from_limbs([907_109, 0, 0, 0]),   // [68]
    U256::from_limbs([903_972, 0, 0, 0]),   // [69]
    U256::from_limbs([900_884, 0, 0, 0]),   // [70]
    U256::from_limbs([897_854, 0, 0, 0]),   // [71]
    U256::from_limbs([894_889, 0, 0, 0]),   // [72]
    U256::from_limbs([891_997, 0, 0, 0]),   // [73]
    U256::from_limbs([889_183, 0, 0, 0]),   // [74]
    U256::from_limbs([886_454, 0, 0, 0]),   // [75]
    U256::from_limbs([883_813, 0, 0, 0]),   // [76]
    U256::from_limbs([881_265, 0, 0, 0]),   // [77]
    U256::from_limbs([878_812, 0, 0, 0]),   // [78]
    U256::from_limbs([876_457, 0, 0, 0]),   // [79]
    U256::from_limbs([874_202, 0, 0, 0]),   // [80]
    U256::from_limbs([872_045, 0, 0, 0]),   // [81]
    U256::from_limbs([869_989, 0, 0, 0]),   // [82]
    U256::from_limbs([868_031, 0, 0, 0]),   // [83]
    U256::from_limbs([866_171, 0, 0, 0]),   // [84]
    U256::from_limbs([864_408, 0, 0, 0]),   // [85]
    U256::from_limbs([862_738, 0, 0, 0]),   // [86]
    U256::from_limbs([861_160, 0, 0, 0]),   // [87]
    U256::from_limbs([859_672, 0, 0, 0]),   // [88]
    U256::from_limbs([858_269, 0, 0, 0]),   // [89]
    U256::from_limbs([856_949, 0, 0, 0]),   // [90]
    U256::from_limbs([855_709, 0, 0, 0]),   // [91]
    U256::from_limbs([854_545, 0, 0, 0]),   // [92]
    U256::from_limbs([853_454, 0, 0, 0]),   // [93]
    U256::from_limbs([852_432, 0, 0, 0]),   // [94]
    U256::from_limbs([851_476, 0, 0, 0]),   // [95]
    U256::from_limbs([850_583, 0, 0, 0]),   // [96]
    U256::from_limbs([849_748, 0, 0, 0]),   // [97]
    U256::from_limbs([848_970, 0, 0, 0]),   // [98]
    U256::from_limbs([848_244, 0, 0, 0]),   // [99]
    U256::from_limbs([847_568, 0, 0, 0]),   // [100]
    U256::from_limbs([846_939, 0, 0, 0]),   // [101]
    U256::from_limbs([846_353, 0, 0, 0]),   // [102]
    U256::from_limbs([845_809, 0, 0, 0]),   // [103]
    U256::from_limbs([845_302, 0, 0, 0]),   // [104]
    U256::from_limbs([844_832, 0, 0, 0]),   // [105]
    U256::from_limbs([844_396, 0, 0, 0]),   // [106]
    U256::from_limbs([843_991, 0, 0, 0]),   // [107]
    U256::from_limbs([843_615, 0, 0, 0]),   // [108]
    U256::from_limbs([843_266, 0, 0, 0]),   // [109]
    U256::from_limbs([842_943, 0, 0, 0]),   // [110]
    U256::from_limbs([842_644, 0, 0, 0]),   // [111]
    U256::from_limbs([842_366, 0, 0, 0]),   // [112]
    U256::from_limbs([842_109, 0, 0, 0]),   // [113]
    U256::from_limbs([841_871, 0, 0, 0]),   // [114]
    U256::from_limbs([841_651, 0, 0, 0]),   // [115]
    U256::from_limbs([841_447, 0, 0, 0]),   // [116]
    U256::from_limbs([841_258, 0, 0, 0]),   // [117]
    U256::from_limbs([841_084, 0, 0, 0]),   // [118]
    U256::from_limbs([840_922, 0, 0, 0]),   // [119]
    U256::from_limbs([840_773, 0, 0, 0]),   // [120]
    U256::from_limbs([840_635, 0, 0, 0]),   // [121]
    U256::from_limbs([840_507, 0, 0, 0]),   // [122]
    U256::from_limbs([840_389, 0, 0, 0]),   // [123]
    U256::from_limbs([840_280, 0, 0, 0]),   // [124]
    U256::from_limbs([840_179, 0, 0, 0]),   // [125]
    U256::from_limbs([840_086, 0, 0, 0]),   // [126]
    U256::from_limbs([840_000, 0, 0, 0]),   // [127]
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
/// exactly `0.84` with floor rounding. The coefficient table is then applied
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
    use outbe_system_common::address_pair::AddressPair;
    use outbe_system_common::units::SCALE_1E6_U64 as COEN840_SCALE;

    use super::*;
    use crate::schema::OracleContract;

    fn price6(whole: u64) -> U256 {
        U256::from(whole) * U256::from(COEN840_SCALE)
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
        // Expected: 18343.66 * 0.999920 = ~18342.19 (approx)
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

    /// 2024-01-01 10:00 UTC, a whole hour.
    const START: u64 = 1_704_103_200;

    fn hour(n: u64) -> u64 {
        START + n * 3_600
    }

    fn day(n: u64) -> u64 {
        START + n * SECONDS_PER_DAY
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
            assert!(store_scurve_entry(oracle, pair, START, price6(500)).unwrap());

            assert_eq!(scurve_value(oracle, pair, START).unwrap(), price6(500));
            assert_eq!(scurve_value(oracle, pair, day(127)).unwrap(), price6(420));
            assert_eq!(scurve_value(oracle, pair, day(128)).unwrap(), price6(420));
        });
    }

    #[test]
    fn a_curve_counts_whole_periods_of_24_h_from_its_start_hour() {
        with_pair(|oracle, pair| {
            store_scurve_entry(oracle, pair, START, price6(100)).unwrap();

            assert_eq!(scurve_value(oracle, pair, START - 1).unwrap(), U256::ZERO);
            assert_eq!(scurve_value(oracle, pair, hour(23)).unwrap(), price6(100));
            assert_eq!(scurve_value(oracle, pair, day(1) - 1).unwrap(), price6(100));
            assert_eq!(
                scurve_value(oracle, pair, day(1)).unwrap(),
                compute_scurve_value(price6(100), 1)
            );
        });
    }

    #[test]
    fn a_price_at_or_below_the_curve_starts_no_curve() {
        with_pair(|oracle, pair| {
            store_scurve_entry(oracle, pair, START, price6(100)).unwrap();

            assert!(!store_scurve_entry(oracle, pair, day(10), price6(90)).unwrap());
            assert!(!store_scurve_entry(oracle, pair, START, price6(200)).unwrap());
            // At day 128 the curve is at 84, so 85 starts a curve below the old peak.
            assert!(store_scurve_entry(oracle, pair, day(128), price6(85)).unwrap());
            assert_eq!(curve(oracle, pair).unwrap(), Some((day(128), price6(85))));
        });
    }

    #[test]
    fn a_start_off_the_hour_grid_is_rejected() {
        with_pair(|oracle, pair| {
            assert!(store_scurve_entry(oracle, pair, START + 1, price6(100)).is_err());
            assert!(store_scurve_entry(oracle, pair, 0, price6(100)).is_err());
        });
    }

    #[test]
    fn curves_are_isolated_by_pair() {
        with_pair(|oracle, usd| {
            let eur = register_test_pair_with_iso(oracle, 978);
            store_scurve_entry(oracle, usd, START, price6(100)).unwrap();
            store_scurve_entry(oracle, eur, START, price6(200)).unwrap();
            store_scurve_entry(oracle, usd, day(1), price6(150)).unwrap();

            assert_eq!(scurve_value(oracle, usd, day(1)).unwrap(), price6(150));
            assert_eq!(
                scurve_value(oracle, eur, day(1)).unwrap(),
                compute_scurve_value(price6(200), 1)
            );
        });
    }

    #[test]
    fn scurve_peaks_from_the_24_hour_vwap() {
        with_pair(|oracle, usd| {
            let cad = register_test_pair_with_iso(oracle, 124);

            // The first 24-hour VWAP of a reference currency starts its curve.
            // CAD is not a reference currency.
            process_hourly_scurves(oracle, hour(0), &[(usd, price6(100)), (cad, price6(100))])
                .unwrap();
            assert_eq!(curve(oracle, usd).unwrap(), Some((hour(0), price6(100))));
            assert_eq!(curve(oracle, cad).unwrap(), None);

            // A VWAP at or below the curve starts no curve.
            process_hourly_scurves(oracle, hour(1), &[(usd, price6(100))]).unwrap();
            assert_eq!(curve(oracle, usd).unwrap(), Some((hour(0), price6(100))));

            // A VWAP above the curve starts a curve at that hour. At its start
            // hour the curve has the value of that VWAP.
            process_hourly_scurves(oracle, hour(2), &[(usd, price6(105))]).unwrap();
            assert_eq!(curve(oracle, usd).unwrap(), Some((hour(2), price6(105))));
            assert_eq!(scurve_value(oracle, usd, hour(2)).unwrap(), price6(105));

            // Each pair keeps one curve.
            assert_eq!(get_all_scurve_data(oracle).unwrap().0.len(), 1);
        });
    }
}
