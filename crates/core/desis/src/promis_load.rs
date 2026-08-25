//! PROMIS load pegged to the COEN/USD rate.
//!
//! One Intex strikes at `promis_load` COEN, so a fixed load is a fixed ticket
//! price only while the rate holds. The load instead tracks the rate down the
//! decades, keeping `load × rate` at the anchor, with a deadband around each
//! boundary so a rate loitering there does not flip the load day after day.

use alloy_primitives::U256;

use crate::constants::{PROMIS_LOAD_ANCHOR_DIGITS, PROMIS_LOAD_BAND_BPS};

const BPS_DEN: u32 = 10_000;

/// Widest load this module will pick: the rate that earns it, `1` minor, is the
/// smallest one the six-decimal COEN/ISO contract can carry.
const MAX_EXPONENT: u32 = PROMIS_LOAD_ANCHOR_DIGITS - 1;

const POW10: [u128; PROMIS_LOAD_ANCHOR_DIGITS as usize + 1] = {
    let mut table = [1u128; PROMIS_LOAD_ANCHOR_DIGITS as usize + 1];
    let mut i = 1;
    while i < table.len() {
        table[i] = table[i - 1] * 10;
        i += 1;
    }
    table
};

/// `promis_load_minor` for exponent `k`.
pub(crate) fn load_minor(exponent: u32) -> u128 {
    POW10[exponent.min(MAX_EXPONENT) as usize]
}

/// Decimal digits of `rate`, saturating at the table's width. Zero has none.
fn decimal_digits(rate: U256) -> u32 {
    if rate.is_zero() {
        return 0;
    }
    let mut digits = 1u32;
    while (digits as usize) < POW10.len() && rate >= U256::from(POW10[digits as usize]) {
        digits += 1;
    }
    digits
}

/// The exponent the anchor alone picks for `rate`, ignoring where the load
/// currently sits: it puts `load × rate` in the anchor's decade.
fn target_exponent(rate: U256) -> u32 {
    PROMIS_LOAD_ANCHOR_DIGITS
        .saturating_sub(decimal_digits(rate))
        .min(MAX_EXPONENT)
}

/// The exponent for a day quoted at `coen_usd_rate_minor`, given the one the
/// chain is on. `current` is `None` on a chain that has never set it, which
/// takes the anchor's answer outright — there is no decade to hold on to.
///
/// A held exponent survives while the rate stays inside its own decade widened
/// by `PROMIS_LOAD_BAND_BPS` at both edges; outside it, the anchor decides
/// again. The edges are compared scaled up rather than divided down, so the
/// band does not round away in the narrow decades.
pub(crate) fn resolve(current: Option<u32>, coen_usd_rate_minor: U256) -> u32 {
    let Some(exponent) = current else {
        return target_exponent(coen_usd_rate_minor);
    };
    let exponent = exponent.min(MAX_EXPONENT);
    let decade = PROMIS_LOAD_ANCHOR_DIGITS - exponent;
    let scaled = coen_usd_rate_minor * U256::from(BPS_DEN);
    let lo = U256::from(POW10[decade as usize - 1]) * U256::from(BPS_DEN - PROMIS_LOAD_BAND_BPS);
    let hi = U256::from(POW10[decade as usize]) * U256::from(BPS_DEN + PROMIS_LOAD_BAND_BPS);
    if scaled >= lo && scaled < hi {
        exponent
    } else {
        target_exponent(coen_usd_rate_minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch decade: 100 000 PROMIS at COEN/USD = 0.001.
    const LAUNCH: u32 = 11;

    fn rate(minor: u64) -> U256 {
        U256::from(minor)
    }

    /// The anchor in six-decimal USD: `load × rate / 1e6`.
    fn strike(rate_minor: u64, load: u128) -> u128 {
        load * u128::from(rate_minor) / 1_000_000
    }

    #[test]
    fn the_anchor_holds_the_strike_across_the_decades() {
        for (rate_minor, expected_load) in [
            (100u64, 1_000_000_000_000u128),
            (1_000, 100_000_000_000),
            (10_000, 10_000_000_000),
            (100_000, 1_000_000_000),
            (1_000_000, 100_000_000),
        ] {
            let load = load_minor(resolve(None, rate(rate_minor)));
            assert_eq!(load, expected_load, "load at rate {rate_minor}");
            assert_eq!(strike(rate_minor, load), 100_000_000, "strike at {rate_minor}");
        }
    }

    #[test]
    fn a_cold_chain_takes_the_anchors_answer() {
        assert_eq!(resolve(None, rate(1_000)), LAUNCH);
        assert_eq!(resolve(None, rate(10_000)), LAUNCH - 1);
    }

    #[test]
    fn the_load_steps_down_only_past_the_widened_upper_edge() {
        // The 0.01 boundary sits at 10_000; the band pushes the step to 10_200.
        assert_eq!(resolve(Some(LAUNCH), rate(10_000)), LAUNCH);
        assert_eq!(resolve(Some(LAUNCH), rate(10_199)), LAUNCH);
        assert_eq!(resolve(Some(LAUNCH), rate(10_200)), LAUNCH - 1);
    }

    #[test]
    fn the_load_steps_up_only_below_the_widened_lower_edge() {
        // Coming from the decade above, the same boundary releases at 9_800.
        assert_eq!(resolve(Some(LAUNCH - 1), rate(10_000)), LAUNCH - 1);
        assert_eq!(resolve(Some(LAUNCH - 1), rate(9_800)), LAUNCH - 1);
        assert_eq!(resolve(Some(LAUNCH - 1), rate(9_799)), LAUNCH);
    }

    #[test]
    fn the_deadband_is_held_from_whichever_side_the_chain_arrived() {
        for rate_minor in [9_800u64, 10_000, 10_199] {
            assert_eq!(resolve(Some(LAUNCH), rate(rate_minor)), LAUNCH);
            assert_eq!(resolve(Some(LAUNCH - 1), rate(rate_minor)), LAUNCH - 1);
        }
    }

    #[test]
    fn a_rate_that_gaps_several_decades_lands_where_the_anchor_says() {
        assert_eq!(resolve(Some(LAUNCH), rate(1_000_000)), LAUNCH - 3);
        assert_eq!(resolve(Some(LAUNCH), rate(1)), LAUNCH + 3);
    }

    #[test]
    fn the_band_survives_integer_division_in_every_decade() {
        for exponent in 1..=MAX_EXPONENT {
            let decade = (PROMIS_LOAD_ANCHOR_DIGITS - exponent) as usize;
            let boundary = U256::from(POW10[decade]);
            // Smallest rate strictly past the widened edge.
            let past = U256::from(POW10[decade])
                * U256::from(BPS_DEN + PROMIS_LOAD_BAND_BPS)
                / U256::from(BPS_DEN)
                + U256::from(1);
            assert_eq!(resolve(Some(exponent), boundary), exponent, "held at 10^{decade}");
            assert!(resolve(Some(exponent), past) < exponent, "stepped past 10^{decade}");
        }
    }

    #[test]
    fn an_unpriced_rate_saturates_to_the_widest_load() {
        // The brief keeps the stored exponent rather than calling this, but the
        // arithmetic must not panic on the way.
        assert_eq!(target_exponent(U256::ZERO), MAX_EXPONENT);
        assert_eq!(load_minor(MAX_EXPONENT), 100_000_000_000_000);
    }

    #[test]
    fn an_absurd_rate_saturates_instead_of_underflowing() {
        let huge = U256::from(u128::MAX);
        assert_eq!(target_exponent(huge), 0);
        assert_eq!(load_minor(target_exponent(huge)), 1);
    }

    #[test]
    fn a_corrupt_stored_exponent_cannot_index_past_the_table() {
        assert_eq!(resolve(Some(u32::MAX), rate(1_000)), LAUNCH);
    }
}
