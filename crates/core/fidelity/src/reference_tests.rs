//! End-to-end golden test: replays the `reference/decay.py` scenario through the
//! Rust cohort engine and checks RCFI / efficiency / d_age against the committed
//! float-model vectors in `tests/fixtures/rcfi_golden.json`.
//!
//! Tolerances (±1 decayed day, ±1e-3 efficiency) reflect the measured
//! float-vs-fixed-point agreement (≈0.035 days / 3e-5). Regenerate the fixture
//! with `python3 reference/decay.py --emit-golden > tests/fixtures/rcfi_golden.json`.

use crate::schema::FidelityContract;
use alloy_primitives::{address, Address, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

const ALICE: Address = address!("0x1111111111111111111111111111111111111111");

struct Tx {
    ts: u64,
    deposit: bool,
    amount: U256,
}

struct Sample {
    ts: u64,
    rcfi: f64,
    efficiency: f64,
    d_age: f64,
}

fn load_fixture() -> (Vec<Tx>, Vec<Sample>) {
    let raw = include_str!("../tests/fixtures/rcfi_golden.json");
    let v: serde_json::Value = serde_json::from_str(raw).expect("valid golden json");

    let txs = v["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| Tx {
            ts: t["ts"].as_u64().unwrap(),
            deposit: t["kind"].as_str().unwrap() == "deposit",
            amount: t["amount_e18"].as_str().unwrap().parse::<U256>().unwrap(),
        })
        .collect();

    let samples = v["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| Sample {
            ts: s["ts"].as_u64().unwrap(),
            rcfi: s["rcfi"].as_f64().unwrap(),
            efficiency: s["efficiency"].as_f64().unwrap(),
            d_age: s["d_age"].as_f64().unwrap(),
        })
        .collect();

    (txs, samples)
}

/// 1e18-scaled fixed-point → f64 (via micro-units to avoid precision loss/overflow).
fn fp_to_f64(fp: U256) -> f64 {
    let micros: u128 = (fp / U256::from(1_000_000_000_000u128)).to::<u128>();
    micros as f64 / 1_000_000.0
}

#[test]
fn golden_matches_decay_py_reference() {
    let (txs, samples) = load_fixture();
    assert!(!samples.is_empty());

    for sample in &samples {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let mut c = FidelityContract::new(storage.clone());
            // Apply every transaction up to and including the sample instant,
            // mirroring the reference's `tx.date <= current_date` loop.
            for tx in &txs {
                if tx.ts <= sample.ts {
                    if tx.deposit {
                        c.on_gratis_mined(ALICE, tx.amount, tx.ts).unwrap();
                    } else {
                        c.on_coen_mined(ALICE, tx.amount, tx.ts).unwrap();
                    }
                }
            }

            let (rcfi_fp, eff_fp, dage_fp) = c.compute_rcfi_fp(ALICE, sample.ts).unwrap();
            let rcfi = fp_to_f64(rcfi_fp);
            let eff = fp_to_f64(eff_fp);
            let dage = fp_to_f64(dage_fp);

            assert!(
                (rcfi - sample.rcfi).abs() <= 1.0,
                "rcfi at ts={}: got {rcfi}, want {} (Δ>1 day)",
                sample.ts,
                sample.rcfi
            );
            assert!(
                (eff - sample.efficiency).abs() <= 1e-3,
                "efficiency at ts={}: got {eff}, want {}",
                sample.ts,
                sample.efficiency
            );
            assert!(
                (dage - sample.d_age).abs() <= 1.0,
                "d_age at ts={}: got {dage}, want {}",
                sample.ts,
                sample.d_age
            );

            // The public u64 RCFI is the floor of the reference within ±1 day.
            let rcfi_u64 = c.compute_rcfi(ALICE, sample.ts).unwrap();
            assert!(
                (rcfi_u64 as f64 - sample.rcfi.floor()).abs() <= 1.0,
                "floor rcfi at ts={}: got {rcfi_u64}, want {}",
                sample.ts,
                sample.rcfi.floor()
            );
        });
    }
}
