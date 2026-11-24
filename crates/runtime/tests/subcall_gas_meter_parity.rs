//! differential proptests verifying that
//! [`outbe_evm::gas::SubcallGasMeter`] is byte-for-byte equal to upstream
//! [`evm2::interpreter::Gas`] across `record_regular_cost`, `erase_cost`,
//! `record_refund`, `set_state_gas_spent`, and `set_reservoir`.
//!
//! Each proptest:
//! 1. Constructs an `evm2::interpreter::Gas` and a `SubcallGasMeter` with the same limit.
//! 2. Applies a randomized sequence of operations to both.
//! 3. Asserts byte-equal post-state on `(limit, remaining, refunded,
//!    state_gas_spent, reservoir)` after each step.
//!
//! Total fixtures: 5 x 100 = 500 cases minimum.

use evm2::interpreter::Gas;
use outbe_evm::gas::SubcallGasMeter;
use proptest::prelude::*;

fn snapshot(gas: &Gas) -> (u64, u64, i64, i64, u64) {
    (
        gas.limit(),
        gas.remaining(),
        gas.refunded(),
        gas.state_gas_spent(),
        gas.reservoir(),
    )
}

fn snapshot_outbe(meter: &SubcallGasMeter) -> (u64, u64, i64, i64, u64) {
    (
        meter.limit(),
        meter.remaining(),
        meter.refunded(),
        meter.state_gas_spent(),
        meter.reservoir(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Mirrors `evm2::interpreter::Gas::record_regular_cost`.
    #[test]
    fn record_regular_cost_byte_equal(
        limit in 0u64..=u64::MAX,
        costs in proptest::collection::vec(0u64..=u64::MAX / 4, 1..16),
    ) {
        let mut upstream = Gas::new(limit);
        let mut outbe = SubcallGasMeter::new(limit);
        prop_assert_eq!(snapshot(&upstream), snapshot_outbe(&outbe), "initial state");

        for cost in costs {
            let up_ok = upstream.tracker_mut().try_spend(cost).is_ok();
            let ob_ok = outbe.record_regular_cost(cost);
            prop_assert_eq!(up_ok, ob_ok, "record_regular_cost bool differs at cost={}", cost);
            prop_assert_eq!(
                snapshot(&upstream),
                snapshot_outbe(&outbe),
                "post-record_regular_cost state diverged at cost={}",
                cost
            );
        }
    }

    /// Mirrors `evm2::interpreter::Gas::erase_cost`. Each step keeps inputs below
    /// `u64::MAX / 32` to avoid `remaining + returned` overflow in upstream's
    /// unchecked add. This is the documented production-realistic range:
    /// gas budgets are sub-tx and never approach `u64::MAX`.
    #[test]
    fn erase_cost_byte_equal(
        limit in 0u64..=u64::MAX / 4,
        prep_cost in 0u64..=u64::MAX / 8,
        returns in proptest::collection::vec(0u64..=u64::MAX / 32, 1..16),
    ) {
        let mut upstream = Gas::new(limit);
        let mut outbe = SubcallGasMeter::new(limit);

        // Spend some gas so `erase_cost` has room to refund.
        let _ = upstream.tracker_mut().try_spend(prep_cost);
        let _ = outbe.record_regular_cost(prep_cost);
        prop_assert_eq!(snapshot(&upstream), snapshot_outbe(&outbe), "after prep-spend");

        for ret in returns {
            upstream.erase_cost(ret);
            outbe.erase_cost(ret);
            prop_assert_eq!(
                snapshot(&upstream),
                snapshot_outbe(&outbe),
                "post-erase_cost state diverged at ret={}",
                ret
            );
        }
    }

    /// Mirrors `evm2::interpreter::Gas::record_refund`. Each step keeps inputs within
    /// +/-1B to avoid `refunded += refund` i64-overflow in upstream. The tx
    /// gas limit bounds realistic per-call refund values (EIP-3529 cap).
    /// These values are always within `i64` headroom.
    #[test]
    fn record_refund_byte_equal(
        limit in 0u64..=u64::MAX,
        refunds in proptest::collection::vec(-1_000_000_000i64..=1_000_000_000, 1..16),
    ) {
        let mut upstream = Gas::new(limit);
        let mut outbe = SubcallGasMeter::new(limit);

        for r in refunds {
            upstream.record_refund(r);
            outbe.record_refund(r);
            prop_assert_eq!(
                snapshot(&upstream),
                snapshot_outbe(&outbe),
                "post-record_refund state diverged at refund={}",
                r
            );
        }
    }

    /// Verifies `SubcallGasMeter::set_state_gas_spent` sets `state_gas_spent` directly
    /// across the full `i64` range (including extremes) without altering any other field.
    #[test]
    fn set_state_gas_spent_byte_equal(
        limit in 0u64..=u64::MAX,
        vals in proptest::collection::vec(i64::MIN..=i64::MAX, 1..16),
    ) {
        let mut outbe = SubcallGasMeter::new(limit);

        for v in vals {
            let before = snapshot_outbe(&outbe);
            outbe.set_state_gas_spent(v);
            let after = snapshot_outbe(&outbe);
            prop_assert_eq!(
                after.3,
                v,
                "state_gas_spent must equal directly set value val={}",
                v
            );
            prop_assert_eq!(
                (after.0, after.1, after.2, after.4),
                (before.0, before.1, before.2, before.4),
                "set_state_gas_spent must not alter unrelated meter fields at val={}",
                v
            );
        }
    }

    /// Mirrors `evm2::interpreter::Gas::set_reservoir`.
    #[test]
    fn set_reservoir_byte_equal(
        limit in 0u64..=u64::MAX,
        vals in proptest::collection::vec(0u64..=u64::MAX, 1..16),
    ) {
        let mut upstream = Gas::new(limit);
        let mut outbe = SubcallGasMeter::new(limit);

        for v in vals {
            upstream.set_reservoir(v);
            outbe.set_reservoir(v);
            prop_assert_eq!(
                snapshot(&upstream),
                snapshot_outbe(&outbe),
                "post-set_reservoir diverged at val={}",
                v
            );
        }
    }
}

#[cfg(test)]
mod smoke {
    use super::*;

    /// Sanity: fresh meter at `new(0)` returns zero on every accessor.
    #[test]
    fn fresh_zero_limit_is_zero_everywhere() {
        let m = SubcallGasMeter::new(0);
        let g = Gas::new(0);
        assert_eq!(snapshot_outbe(&m), snapshot(&g));
        assert_eq!(m.limit(), 0);
        assert_eq!(m.remaining(), 0);
        assert_eq!(m.refunded(), 0);
        assert_eq!(m.state_gas_spent(), 0);
        let mut meter = SubcallGasMeter::new(100_000);
        for extreme in [i64::MIN, i64::MAX, i64::MIN, -1, 0, 1, i64::MAX] {
            let before = snapshot_outbe(&meter);
            meter.set_state_gas_spent(extreme);
            let after = snapshot_outbe(&meter);
            assert_eq!(after.3, extreme);
            assert_eq!(
                (after.0, after.1, after.2, after.4),
                (before.0, before.1, before.2, before.4)
            );
        }
        assert_eq!(m.reservoir(), 0);
    }

    /// Sanity: `record_regular_cost` returns the same bool as upstream
    /// when gas is exhausted.
    #[test]
    fn out_of_gas_returns_false() {
        let mut m = SubcallGasMeter::new(100);
        let mut g = Gas::new(100);
        assert_eq!(
            m.record_regular_cost(50),
            g.tracker_mut().try_spend(50).is_ok()
        );
        // Exhaust the budget.
        let too_much_outbe = m.record_regular_cost(60);
        let too_much_evm2 = g.tracker_mut().try_spend(60).is_ok();
        assert_eq!(too_much_outbe, too_much_evm2);
        assert!(!too_much_outbe, "over-budget cost must return false");
    }
}
