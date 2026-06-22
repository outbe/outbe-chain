use alloy_primitives::{address, Address, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::math::SCALE;
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
