use alloy_primitives::{address, Address, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::math::{t_dec, L_FP, SCALE};
use crate::schema::{active_cohort_key, sold_cohort_key, FidelityContract};
use crate::schema::{ActiveCohort, SoldCohort};

const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const DAY: u64 = 86_400;
const T0: u64 = 1_700_000_000;

fn with_contract<R>(f: impl FnOnce(&mut FidelityContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = FidelityContract::new(storage.clone());
        f(&mut contract)
    })
}

fn active(c: &FidelityContract, owner: Address, i: u32) -> Option<ActiveCohort> {
    c.active_cohorts.get(active_cohort_key(owner, i)).unwrap()
}

fn sold(c: &FidelityContract, owner: Address, i: u32) -> Option<SoldCohort> {
    c.sold_cohorts.get(sold_cohort_key(owner, i)).unwrap()
}

fn u(v: u64) -> U256 {
    U256::from(v)
}

// --- RCFI cohort engine -------------------------------------------------

#[test]
fn no_history_returns_zero_rcfi() {
    with_contract(|c| {
        // get_rcfi reads the (zero) block timestamp; no cohorts → 0.
        assert_eq!(c.get_rcfi(ALICE).unwrap(), 0);
    });
}

#[test]
fn single_cohort_decay_matches_spec() {
    // 100% holding → RCFI == T_dec(wallet_age). Spec checkpoints: 263 @ 1yr,
    // 493 @ 4yr (the PDF reference benchmark ≈ 493.67).
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        assert_eq!(c.compute_rcfi(ALICE, T0 + 365 * DAY).unwrap(), 263);
        assert_eq!(c.compute_rcfi(ALICE, T0 + 1460 * DAY).unwrap(), 493);
    });
}

#[test]
fn deposits_keep_efficiency_one() {
    // Balance independence: replenishments (any size) never lower efficiency,
    // and RCFI equals the decayed wallet age.
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        c.cohort_in(ALICE, u(5), T0 + 200 * DAY).unwrap();
        c.cohort_in(ALICE, u(1000), T0 + 300 * DAY).unwrap();
        let (rcfi, eff, dage) = c.compute_rcfi_fp(ALICE, T0 + 500 * DAY).unwrap();
        assert_eq!(eff, SCALE, "efficiency must be exactly 1.0 with no sales");
        assert_eq!(rcfi, dage, "RCFI == d_dec_age when efficiency == 1");
    });
}

#[test]
fn partial_sale_splits_boundary_cohort() {
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        let sell_ts = T0 + 10 * DAY;
        c.cohort_out(ALICE, u(30), sell_ts).unwrap();

        // Active remainder {70, T0}; sold slice {30, T0, sell_ts}.
        assert_eq!(c.active_count.read(&ALICE).unwrap(), 1);
        let a0 = active(c, ALICE, 0).unwrap();
        assert_eq!(a0.size, u(70));
        assert_eq!(a0.acquired_at, T0);
        assert_eq!(c.sold_count.read(&ALICE).unwrap(), 1);
        let s0 = sold(c, ALICE, 0).unwrap();
        assert_eq!(s0.size, u(30));
        assert_eq!(s0.acquired_at, T0);
        assert_eq!(s0.sold_at, sell_ts);

        // Sell the remaining 70 fully → active empty, two sold slices.
        let sell2 = T0 + 20 * DAY;
        c.cohort_out(ALICE, u(70), sell2).unwrap();
        assert_eq!(c.active_count.read(&ALICE).unwrap(), 0);
        assert!(active(c, ALICE, 0).is_none());
        assert_eq!(c.sold_count.read(&ALICE).unwrap(), 2);
        let s1 = sold(c, ALICE, 1).unwrap();
        assert_eq!(s1.size, u(70));
        assert_eq!(s1.acquired_at, T0);
        assert_eq!(s1.sold_at, sell2);
    });
}

#[test]
fn lifo_consumes_youngest_active_first() {
    with_contract(|c| {
        c.cohort_in(ALICE, u(10), T0).unwrap(); // idx0 (oldest)
        c.cohort_in(ALICE, u(20), T0 + DAY).unwrap(); // idx1
        c.cohort_in(ALICE, u(30), T0 + 2 * DAY).unwrap(); // idx2 (youngest)

        // Sell 35: consumes idx2 (30) fully, then 5 from idx1 (split).
        let sell_ts = T0 + 3 * DAY;
        c.cohort_out(ALICE, u(35), sell_ts).unwrap();

        assert_eq!(c.active_count.read(&ALICE).unwrap(), 2);
        assert_eq!(active(c, ALICE, 0).unwrap().size, u(10)); // oldest untouched
        assert_eq!(active(c, ALICE, 1).unwrap().size, u(15)); // 20 - 5
        assert!(active(c, ALICE, 2).is_none());

        assert_eq!(c.sold_count.read(&ALICE).unwrap(), 2);
        let s0 = sold(c, ALICE, 0).unwrap(); // youngest sold first
        assert_eq!(s0.size, u(30));
        assert_eq!(s0.acquired_at, T0 + 2 * DAY);
        let s1 = sold(c, ALICE, 1).unwrap();
        assert_eq!(s1.size, u(5));
        assert_eq!(s1.acquired_at, T0 + DAY);
    });
}

#[test]
fn large_mature_sale_halves_then_recovers() {
    // Two equal mature cohorts; selling one (LIFO) drops efficiency to exactly
    // 0.5 at the sale instant, then it recovers above 0.5 as the sale is forgotten.
    with_contract(|c| {
        c.cohort_in(ALICE, u(500), T0).unwrap();
        c.cohort_in(ALICE, u(500), T0).unwrap(); // identical age
        let sell_ts = T0 + 1000 * DAY;
        c.cohort_out(ALICE, u(500), sell_ts).unwrap();

        let (_, eff_at_sale, _) = c.compute_rcfi_fp(ALICE, sell_ts).unwrap();
        assert_eq!(
            eff_at_sale,
            SCALE / u(2),
            "efficiency exactly halves at sale"
        );

        let (_, eff_later, _) = c.compute_rcfi_fp(ALICE, sell_ts + 1000 * DAY).unwrap();
        assert!(eff_later > SCALE / u(2), "sale fades → efficiency recovers");
    });
}

#[test]
fn fresh_large_sale_is_negligible() {
    // Selling a just-acquired cohort (LIFO) leaves efficiency at 1.0 — the sold
    // slice has ~0 decayed duration.
    with_contract(|c| {
        c.cohort_in(ALICE, u(1000), T0).unwrap();
        let t1 = T0 + 1000 * DAY;
        c.cohort_in(ALICE, u(1000), t1).unwrap(); // fresh
        c.cohort_out(ALICE, u(1000), t1).unwrap(); // sold immediately

        let (rcfi, eff, dage) = c.compute_rcfi_fp(ALICE, t1).unwrap();
        assert_eq!(eff, SCALE);
        assert_eq!(rcfi, dage);
    });
}

#[test]
fn small_youngest_sale_is_negligible() {
    // LIFO sells the youngest cohort first; a small young slice has tiny decayed
    // duration, so efficiency barely moves (< 0.1%).
    with_contract(|c| {
        c.cohort_in(ALICE, u(1000), T0).unwrap(); // old core
        let young_ts = T0 + 1400 * DAY;
        c.cohort_in(ALICE, u(5), young_ts).unwrap();
        let sell_ts = young_ts + DAY;
        c.cohort_out(ALICE, u(1), sell_ts).unwrap();

        let (_, eff, _) = c.compute_rcfi_fp(ALICE, sell_ts).unwrap();
        let drop = SCALE - eff;
        assert!(drop < SCALE / u(1000), "efficiency dropped too much: {eff}");
    });
}

#[test]
fn sale_exceeding_cohorts_clamps() {
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        let sell_ts = T0 + DAY;
        // Selling more than recorded must not panic/revert.
        c.cohort_out(ALICE, u(250), sell_ts).unwrap();

        assert_eq!(c.active_count.read(&ALICE).unwrap(), 0);
        assert!(active(c, ALICE, 0).is_none());
        assert_eq!(c.sold_count.read(&ALICE).unwrap(), 1);
        assert_eq!(sold(c, ALICE, 0).unwrap().size, u(100)); // excess 150 ignored
        assert_eq!(c.qualified_start.read(&ALICE).unwrap(), T0);

        // Fully sold out → efficiency and RCFI are zero.
        let (rcfi, eff, _) = c.compute_rcfi_fp(ALICE, T0 + 10 * DAY).unwrap();
        assert_eq!(eff, U256::ZERO);
        assert_eq!(rcfi, U256::ZERO);
    });
}

#[test]
fn zero_amount_hooks_are_noops() {
    with_contract(|c| {
        c.cohort_in(ALICE, U256::ZERO, T0).unwrap();
        c.cohort_out(ALICE, U256::ZERO, T0).unwrap();
        assert_eq!(c.active_count.read(&ALICE).unwrap(), 0);
        assert_eq!(c.qualified_start.read(&ALICE).unwrap(), 0);
    });
}

// --- compute_rcfi_scaled: 1e18 fixed-point RCFI at historical timestamps -----

/// Fixed-point (10^18) decayed-days → f64 days, for tolerance checks only.
fn days(fp: U256) -> f64 {
    let micro: u128 = (fp * U256::from(1_000_000u64) / SCALE).to::<u128>();
    micro as f64 / 1_000_000.0
}

#[test]
fn compute_rcfi_scaled_equals_t_dec_for_holding_at_historical_times() {
    // 100% holding (one cohort, no sales) ⇒ efficiency == 1, so the scaled RCFI
    // is exactly the decayed wallet age `t_dec(age)` evaluated AT THE SUPPLIED
    // timestamp — the 1e18 value the precompile returns. Checking several past
    // timestamps proves the result is a pure function of the `timestamp`
    // argument (history evaluated "as of" that instant), not of "now".
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        for age in [0u64, 1, 365, 730, 1460, 3650] {
            let ts = T0 + age * DAY;
            assert_eq!(
                c.compute_rcfi_scaled(ALICE, ts).unwrap(),
                t_dec(age * DAY),
                "scaled RCFI must equal t_dec(age) at +{age}d",
            );
        }
        // A timestamp before the wallet's first acquisition has zero retention.
        assert_eq!(
            c.compute_rcfi_scaled(ALICE, T0 - 100 * DAY).unwrap(),
            U256::ZERO,
            "pre-acquisition timestamp must be zero",
        );
    });
}

#[test]
fn compute_rcfi_scaled_is_unfloored_fp_and_floors_to_compute_rcfi() {
    // Across a history (deposit + later partial sale) and several historical
    // timestamps, the scaled value is the *un-floored* `compute_rcfi_fp` result,
    // and flooring it by SCALE (i.e. `decimals() == 18`) reproduces the
    // integer-day `compute_rcfi`.
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        c.cohort_in(ALICE, u(50), T0 + 100 * DAY).unwrap();
        c.cohort_out(ALICE, u(30), T0 + 200 * DAY).unwrap();

        for age in [50u64, 150, 365, 1000, 1460] {
            let ts = T0 + age * DAY;
            let scaled = c.compute_rcfi_scaled(ALICE, ts).unwrap();
            let (raw_fp, _, _) = c.compute_rcfi_fp(ALICE, ts).unwrap();
            assert_eq!(
                scaled, raw_fp,
                "scaled must equal raw fixed-point at +{age}d"
            );
            assert_eq!(
                (scaled / SCALE).to::<u64>(),
                c.compute_rcfi(ALICE, ts).unwrap(),
                "floor(scaled / 1e18) must equal compute_rcfi at +{age}d",
            );
        }
    });
}

#[test]
fn compute_rcfi_scaled_preserves_subday_precision() {
    // At one year a 100%-held wallet has RCFI ≈ 263.2918 decayed days — not a
    // whole number. The scaled value keeps the fraction the floored `u64`
    // variant discards.
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        let scaled = c.compute_rcfi_scaled(ALICE, T0 + 365 * DAY).unwrap();
        assert_eq!(scaled / SCALE, u(263), "integer part is 263 decayed days");
        assert!(
            scaled % SCALE != U256::ZERO,
            "fractional 1e18 part must be retained"
        );
        assert!(
            (days(scaled) - 263.2918).abs() < 0.001,
            "matches the spec checkpoint",
        );
    });
}

#[test]
fn compute_rcfi_scaled_is_monotonic_across_historical_timestamps() {
    // Querying progressively later historical timestamps yields strictly larger
    // values (decay accumulates) that never exceed the saturation limit L.
    with_contract(|c| {
        c.cohort_in(ALICE, u(100), T0).unwrap();
        let mut prev = U256::ZERO;
        for age in [1u64, 30, 365, 730, 1460, 3650, 36500] {
            let cur = c.compute_rcfi_scaled(ALICE, T0 + age * DAY).unwrap();
            assert!(cur > prev, "scaled RCFI must increase at +{age}d");
            assert!(cur <= L_FP, "scaled RCFI must not exceed L at +{age}d");
            prev = cur;
        }
    });
}

#[test]
fn compute_rcfi_scaled_no_history_is_zero_at_any_timestamp() {
    with_contract(|c| {
        for age in [0u64, 365, 3650] {
            assert_eq!(
                c.compute_rcfi_scaled(ALICE, T0 + age * DAY).unwrap(),
                U256::ZERO,
                "no cohorts ⇒ zero RCFI at +{age}d",
            );
        }
    });
}
