//! The settlement cost of one Nod.

use crate::api::settlement_cost_minor;
use alloy_primitives::U256;

#[test]
fn positive_dust_cost_has_a_one_minor_unit_minimum() {
    // Actual Rudis AmountMap failures and the smallest positive inputs.
    for (price, load) in [(19_u64, 25_629_u64), (19, 1_882), (19, 2_529), (1, 1)] {
        assert_eq!(
            settlement_cost_minor(U256::from(price), U256::from(load)).unwrap(),
            U256::from(1)
        );
    }
}

#[test]
fn non_dust_cost_keeps_floor_rounding() {
    for (price, load, expected) in [
        (19_u64, 52_631_u64, 1_u64),
        (19, 52_632, 1),
        (19, 105_263, 1),
        (19, 105_264, 2),
        (2_000_000, 5_000_000, 10_000_000),
    ] {
        assert_eq!(
            settlement_cost_minor(U256::from(price), U256::from(load)).unwrap(),
            U256::from(expected)
        );
    }
}

#[test]
fn minimum_does_not_mask_zero_inputs_or_overflow() {
    for (price, load) in [(0_u64, 1_u64), (1, 0), (0, 0)] {
        assert_eq!(
            settlement_cost_minor(U256::from(price), U256::from(load)).unwrap(),
            U256::ZERO
        );
    }
    assert!(settlement_cost_minor(U256::MAX, U256::from(2)).is_err());
}
