//! Closed-form two-phase day emission cap.
//!
//! The curve rises from 2^28 COEN to `P` through day 1024, then falls to
//! 2^26 COEN on day 3072. From that day onward it returns the permanent
//! `FLOOR_DAY_EMISSION`.
//!
//! Consensus execution uses deterministic U256 fixed-point arithmetic. An
//! independent Python `Decimal` script generates the committed reference
//! vectors.

pub use outbe_primitives::emission::{
    day_emission_limit, FLOOR_DAY_EMISSION, FLOOR_DAY_THRESHOLD, INITIAL_DAY_EMISSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{uint, U256};
    use outbe_primitives::units::SCALE_1E6_U256;

    const REFERENCE_VECTORS: &str =
        include_str!("../../../../testing/emission-reference/vectors.json");
    const PIN_DAY_0: U256 = uint!(268_435_456_000_000_U256);
    const PIN_DAY_1: U256 = uint!(268_480_452_000_000_U256);
    const PIN_DAY_365: U256 = uint!(340_810_652_000_000_U256);
    const PIN_DAY_512: U256 = uint!(425_022_805_000_000_U256);
    const PIN_DAY_730: U256 = uint!(537_405_916_000_000_U256);
    const PIN_DAY_1024: U256 = uint!(581_610_154_000_000_U256);
    const PIN_DAY_1025: U256 = uint!(581_516_499_000_000_U256);
    const PIN_DAY_1460: U256 = uint!(514_882_469_000_000_U256);
    const PIN_DAY_2048: U256 = uint!(324_359_509_000_000_U256);
    const PIN_DAY_2190: U256 = uint!(270_306_028_000_000_U256);
    const PIN_DAY_2919: U256 = uint!(84_150_920_000_000_U256);
    const PIN_DAY_2920: U256 = uint!(84_020_175_000_000_U256);
    const PIN_DAY_3071: U256 = uint!(67_202_518_000_000_U256);
    const PIN_DAY_3072: U256 = uint!(67_108_864_000_000_U256);

    fn reference_days() -> Vec<(u32, U256)> {
        let value: serde_json::Value =
            serde_json::from_str(REFERENCE_VECTORS).expect("independent emission vectors parse");
        value["days"]
            .as_array()
            .expect("days is an array")
            .iter()
            .map(|row| {
                let day =
                    u32::try_from(row["day"].as_u64().expect("day is u64")).expect("day fits u32");
                let emission = U256::from_str_radix(
                    row["emission_units"]
                        .as_str()
                        .expect("emission_units is a decimal string"),
                    10,
                )
                .expect("emission_units fits U256");
                (day, emission)
            })
            .collect()
    }

    #[test]
    fn founder_curve_checkpoints_match_independent_reference() {
        for (day, expected) in [
            (0, PIN_DAY_0),
            (1, PIN_DAY_1),
            (365, PIN_DAY_365),
            (512, PIN_DAY_512),
            (730, PIN_DAY_730),
            (1024, PIN_DAY_1024),
            (1025, PIN_DAY_1025),
            (1460, PIN_DAY_1460),
            (2048, PIN_DAY_2048),
            (2190, PIN_DAY_2190),
            (2919, PIN_DAY_2919),
            (2920, PIN_DAY_2920),
            (3071, PIN_DAY_3071),
            (3072, PIN_DAY_3072),
        ] {
            assert_eq!(day_emission_limit(day), expected, "day {day}");
        }
    }

    #[test]
    fn floor_clamp_at_and_beyond_threshold() {
        assert_eq!(FLOOR_DAY_THRESHOLD, 3_072);
        assert_eq!(day_emission_limit(FLOOR_DAY_THRESHOLD), PIN_DAY_3072);
        assert_eq!(day_emission_limit(FLOOR_DAY_THRESHOLD + 1), PIN_DAY_3072);
        assert_eq!(day_emission_limit(u32::MAX), PIN_DAY_3072);
    }

    #[test]
    fn independent_reference_is_contiguous_two_phase_and_floor_clamped() {
        let days = reference_days();
        assert_eq!(days.len(), FLOOR_DAY_THRESHOLD as usize + 1);
        for (index, (day, value)) in days.iter().copied().enumerate() {
            assert_eq!(day as usize, index, "reference day sequence");
            if let Some((_, previous)) = index.checked_sub(1).map(|i| days[i]) {
                if day <= 1_024 {
                    assert!(
                        value >= previous,
                        "reference decreases before P at day {day}"
                    );
                } else {
                    assert!(
                        value <= previous,
                        "reference increases after P at day {day}"
                    );
                }
            }
        }
        assert_eq!(
            days.last().copied(),
            Some((FLOOR_DAY_THRESHOLD, PIN_DAY_3072))
        );
    }

    #[test]
    fn production_matches_independent_reference_for_full_range() {
        for (day, expected) in reference_days() {
            let actual = day_emission_limit(day);
            assert_eq!(actual, expected, "day {day}");
            assert_eq!(actual % SCALE_1E6_U256, U256::ZERO, "day {day}");
        }
    }

    #[test]
    fn production_rises_to_p_then_falls_to_the_floor() {
        let mut previous = day_emission_limit(0);
        for day in 1..=FLOOR_DAY_THRESHOLD {
            let current = day_emission_limit(day);
            if day <= 1_024 {
                assert!(
                    current >= previous,
                    "production decreases before P at day {day}"
                );
            } else {
                assert!(
                    current <= previous,
                    "production increases after P at day {day}"
                );
            }
            previous = current;
        }
    }
}
