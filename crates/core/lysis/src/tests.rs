use crate::algorithm::*;
use crate::constants::{F_FP_DEFAULT, F_MAX_FP};
use alloy_primitives::{Address, LogData, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{decode_nod_item_v1, derive_poseidon_entity_id, EntityId36};
use outbe_nod::{from_canonical_item, precompile::INod, NodContract, NodRepositoryReader};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_oracle::contract::OracleContract;
use outbe_primitives::addresses::NOD_ADDRESS;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::units::{Units, SCALE_1E18};
use outbe_tribute::{
    TributeContract, TributeData, TributeRepositoryReader, TributeRepositoryWriter,
};
use std::sync::Arc;

struct TestBodyRepository {
    tribute_reader: TributeRepositoryReader,
    tribute_writer: TributeRepositoryWriter,
    nod_reader: NodRepositoryReader,
}

impl TestBodyRepository {
    fn new() -> Self {
        let storage = Arc::new(MemoryStorage::new());
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage;
        Self {
            tribute_reader: TributeRepositoryReader::new(reader.clone()),
            tribute_writer: TributeRepositoryWriter::new(reader.clone(), writer),
            nod_reader: NodRepositoryReader::new(reader),
        }
    }

    fn issue(&self, contract: &mut TributeContract<'_>, tribute: &TributeData) {
        contract.issue(&self.tribute_reader, tribute).unwrap();
        self.tribute_writer.put(tribute).unwrap();
    }
}

fn gas_audit_address(n: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = 0x22;
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(bytes)
}

fn gas_audit_tribute(
    _tribute_seed: u64,
    owner: Address,
    worldwide_day: WorldwideDay,
    nominal_amount_minor: U256,
) -> TributeData {
    TributeData {
        tribute_id: entity_id(worldwide_day, owner),
        owner,
        worldwide_day,
        issuance_amount_minor: nominal_amount_minor / U256::from(2u64),
        issuance_currency: 1,
        nominal_amount_minor,
        reference_currency: 840,
        exclude_from_intex_issuance: false,
        tribute_price_minor: U256::ZERO,
    }
}

fn entity_id(worldwide_day: WorldwideDay, owner: Address) -> EntityId36 {
    derive_poseidon_entity_id(owner, worldwide_day).unwrap()
}

fn decode_nod_body_event(event: &LogData) -> outbe_nod::NodItemState {
    let decoded = INod::NodBodyStored::decode_log_data(event).unwrap();
    let event_id = EntityId36::try_from(decoded.nodId.as_ref()).unwrap();
    let item = from_canonical_item(decode_nod_item_v1(&decoded.canonicalPayload).unwrap());
    assert_eq!(event_id, item.nod_id);
    item
}

#[test]
fn gas_08_lysis_dense_day_completes_and_emits_body_mutations() {
    const DENSE_TRIBUTE_COUNT: u64 = 512;
    const T_NOW: u64 = 1_700_000_000;
    let wwd = WorldwideDay::new(20260525);
    let nominal = U256::in_units(100u64);
    let total_nominal = nominal * U256::from(DENSE_TRIBUTE_COUNT);
    let gratis_allocation = total_nominal / U256::from(10u64);
    let cost_of_gratis = U256::from(500_000_000_000_000_000u128);
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    let bodies = TestBodyRepository::new();

    let result = StorageHandle::enter(&mut storage, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        // Register ISO 840 (USD) → COEN/0xUSD pair so the runtime's
        // `outbe_oracle::api::get_pair_id(_, 840)` lookup resolves.
        let pair_hash = OracleContract::pair_hash("COEN", "0xUSD");
        oracle
            .settlement_iso_to_pair
            .write(&840u16, pair_hash)
            .unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        oracle
            .worldwide_day_vwap_pair_count
            .write(&wwd, 1u32)
            .unwrap();
        oracle
            .worldwide_day_vwap_pair_id
            .get_nested(&wwd)
            .write(&0u32, pair_id)
            .unwrap();
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&0u32, cost_of_gratis)
            .unwrap();

        let mut tribute = TributeContract::new(storage.clone());
        tribute.unseal_day(wwd).unwrap();
        for token_id in 1..=DENSE_TRIBUTE_COUNT {
            let owner = gas_audit_address(token_id);
            bodies.issue(
                &mut tribute,
                &gas_audit_tribute(token_id, owner, wwd, nominal),
            );
        }
        assert_eq!(
            tribute
                .get_all_day_tributes(&bodies.tribute_reader, wwd)
                .unwrap()
                .len(),
            DENSE_TRIBUTE_COUNT as usize,
            "GAS-08 fixture must seed a dense but valid Lysis day"
        );

        let result = crate::runtime::lysis(
            storage.clone(),
            &bodies.tribute_reader,
            &bodies.nod_reader,
            wwd,
            T_NOW,
            gratis_allocation,
        )
        .expect("GAS-08 dense Lysis day must complete");

        assert_eq!(
            result.tribute_ids.len(),
            DENSE_TRIBUTE_COUNT as usize,
            "GAS-08: dense Lysis should load every tribute in the day"
        );
        assert_eq!(
            result.nod_ids.len(),
            DENSE_TRIBUTE_COUNT as usize,
            "GAS-08: dense Lysis should issue one NOD for every funded tribute"
        );

        let tribute = TributeContract::new(storage.clone());
        assert_eq!(
            tribute.total_supply().unwrap(),
            0,
            "GAS-08: all processed tributes must be burned after dense Lysis"
        );

        let nod = NodContract::new(storage.clone());
        assert_eq!(
            nod.total_supply().unwrap(),
            DENSE_TRIBUTE_COUNT,
            "GAS-08: dense Lysis must persist every issued NOD"
        );
        result
    });

    let stored_items = storage
        .get_events(NOD_ADDRESS)
        .iter()
        .filter(|event| event.topics()[0] == INod::NodBodyStored::SIGNATURE_HASH)
        .map(decode_nod_body_event)
        .collect::<Vec<_>>();
    assert_eq!(stored_items.len(), DENSE_TRIBUTE_COUNT as usize);
    let mut issued_gratis = U256::ZERO;
    for (idx, item) in stored_items.iter().enumerate() {
        assert_eq!(item.owner, gas_audit_address(idx as u64 + 1));
        assert_eq!(item.worldwide_day, wwd);
        assert_eq!(item.league_id, 1);
        assert!(!item.gratis_load_minor.is_zero());
        assert_eq!(
            item.cost_amount_minor,
            cost_of_gratis * item.gratis_load_minor / SCALE_1E18,
        );
        issued_gratis += item.gratis_load_minor;
    }
    assert_eq!(issued_gratis + result.remaining_gratis, gratis_allocation);
}

#[test]
fn test_empty_population() {
    let result = calc_fraction_distribution_fp(&[], &[], 0, F_FP_DEFAULT, F_MAX_FP).unwrap();
    assert_eq!(result, vec![U256::ZERO]);
}

#[test]
fn test_single_fi_returns_target_fraction() {
    let y_fp = vec![SCALE]; // 100%
    let p = vec![5];
    let result = calc_fraction_distribution_fp(&y_fp, &p, 1, F_FP_DEFAULT, F_MAX_FP).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], F_FP_DEFAULT, "single FI should return f");
}

#[test]
fn test_two_fi_groups() {
    let y_fp = vec![
        SCALE * U256::from(6u64) / U256::from(10u64),
        SCALE * U256::from(4u64) / U256::from(10u64),
    ]; // 60/40
    let p = vec![1, 2];
    let result = calc_fraction_distribution_fp(&y_fp, &p, 2, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 2);

    // All fractions non-negative
    for (i, &frac) in result.iter().enumerate() {
        assert!(
            !frac.is_zero(),
            "fraction[{i}] should be positive, got {frac}"
        );
    }

    // Bounded by 2*fmax (reasonable bound for fixed-point)
    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] too large: {frac}");
    }
}

#[test]
fn test_three_fi_groups() {
    let y_fp = vec![
        SCALE * U256::from(50u64) / U256::from(100u64),
        SCALE * U256::from(30u64) / U256::from(100u64),
        SCALE * U256::from(20u64) / U256::from(100u64),
    ];
    let p = vec![50, 30, 20];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 3, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 3);

    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] > bound: {frac}");
    }
}

#[test]
fn test_many_fi_groups() {
    let n = 10;
    let y_fp: Vec<U256> = vec![SCALE / U256::from(n as u64); n];
    let p: Vec<u64> = (1..=n as u64).collect();

    let result = calc_fraction_distribution_fp(&y_fp, &p, 100, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), n);

    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] too large");
    }
}

/// f_fp must be clamped to [LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX/2].
#[test]
fn test_deficit_derivation_scarce_and_abundant() {
    // Scarce: deficit below the 8% floor → f = deficit (adapts down).
    let scarce = F_FP_DEFAULT / U256::from(2u64); // 4%
    let f_fp = scarce.min(F_FP_DEFAULT);
    let fmax_fp = F_MAX_FP.min(f_fp * U256::from(2u64));
    assert_eq!(f_fp, scarce, "scarce gratis must lower the floor below 8%");
    assert_eq!(
        fmax_fp,
        scarce * U256::from(2u64),
        "fmax tracks 2*f when below the cap"
    );

    // Abundant: deficit at/above 8% → f capped at 8%, fmax at 16%.
    let abundant = F_MAX_FP; // 16% deficit
    let f_fp = abundant.min(F_FP_DEFAULT);
    let fmax_fp = F_MAX_FP.min(f_fp * U256::from(2u64));
    assert_eq!(f_fp, F_FP_DEFAULT, "abundant gratis caps the floor at 8%");
    assert_eq!(fmax_fp, F_MAX_FP, "fmax caps at 16%");
}

#[test]
fn test_default_constants() {
    // Verify constants match expected values within integer precision
    assert_eq!(F_FP_DEFAULT, U256::from(320_000_000_000_000_000u128)); // 0.32 * 10^18
    assert_eq!(F_MAX_FP, U256::from(640_000_000_000_000_000u128)); // 0.64 * 10^18
}

#[test]
fn test_with_zero_population_entries() {
    let half = SCALE / U256::from(2u64);
    let y_fp = vec![half, U256::ZERO, half];
    let p = vec![10, 0, 5];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 15, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 3);
    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] > bound: {frac}");
    }
}

#[test]
fn test_skewed_distribution() {
    let y_fp = vec![
        SCALE * U256::from(9u64) / U256::from(10u64),
        SCALE / U256::from(10u64),
    ];
    let p = vec![900, 100];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 1000, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 2);
    assert!(!result[0].is_zero());
    assert!(!result[1].is_zero());
}

/// Regression: large nominal amounts (> 2^53) must not lose precision.
#[test]
fn test_large_nominal_distribution() {
    // Simplified: 60/40 split → use SCALE fractions directly.
    let y_fp = vec![
        SCALE * U256::from(6u64) / U256::from(10u64),
        SCALE * U256::from(4u64) / U256::from(10u64),
    ];
    let p = vec![600, 400];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 1000, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 2);
    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(
            !frac.is_zero(),
            "fraction[{i}] must be positive for large nominals"
        );
        assert!(frac <= bound, "fraction[{i}] must be bounded");
    }
}

// ---------------------------------------------------------------------------
// weighted-expenditure cap invariant
// ---------------------------------------------------------------------------

/// Assert the post-condition `sum(f1[i] * y_fp[i]) / SCALE <= f_fp` for the
/// output of `calc_fraction_distribution_fp`. Small round-down error is
/// acceptable; overshoot is not.
fn assert_weighted_within_target(result: &[U256], y_fp: &[U256], f_fp: U256) {
    let weighted: U256 = result
        .iter()
        .zip(y_fp.iter())
        .map(|(f, y)| *f * *y / SCALE)
        .sum();
    assert!(
        weighted <= f_fp,
        "weighted expenditure {weighted} exceeds target {f_fp}"
    );
}

#[test]
fn test_normalized_f1_respects_budget_skewed_population() {
    // Skewed population + imbalanced interest tends to push raw f1 over the
    // target. After normalization the post-condition must hold.
    let q = SCALE / U256::from(4u64);
    let y_fp = vec![q, q, q, q];
    let p = vec![100u64, 1, 1, 1];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 103, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 4);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_normalized_f1_respects_budget_many_groups() {
    let n = 10usize;
    let y_fp: Vec<U256> = (0..n).map(|_| SCALE / U256::from(n as u64)).collect();
    let p: Vec<u64> = (1..=n as u64).collect();
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 100, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), n);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_single_group_returns_f_without_normalization() {
    // The single-group fast path bypasses the normalization loop; `f_fp` is
    // returned as-is. Weighted total = f_fp * SCALE / SCALE = f_fp == target.
    let y_fp = vec![SCALE];
    let p = vec![10];
    let f_fp = F_FP_DEFAULT;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, f_fp, F_MAX_FP).unwrap();
    assert_eq!(result, vec![f_fp]);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_normalized_f1_preserves_ratios_when_scaled_down() {
    // When raw output overshoots and is scaled down, pairwise ratios between
    // groups should remain ~constant.
    let half = SCALE / U256::from(2u64);
    let y_fp = vec![half, half];
    let p = vec![50u64, 5];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 55, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    assert_weighted_within_target(&result, &y_fp, f_fp);
    // Both fractions should still be positive (not obliterated by scale-down).
    for &frac in &result {
        assert!(
            !frac.is_zero(),
            "fraction must remain positive after normalization"
        );
    }
}

// ---------------------------------------------------------------------------
// I256 precision — no silent zero-collapse on small FI groups
// ---------------------------------------------------------------------------

/// Input with a dominant group and one tiny-interest group. Under the
/// pre- i128 pipeline with `/1_000_000` scale-down the small group's
/// `f1` could collapse to 0 (up to 10^6 SCALE units of precision lost per
/// term). After I256 refactor the distribution must preserve the signal.
#[test]
fn test_small_fi_group_survives_i256_precision() {
    let tiny = U256::from(1_000_000u64);
    let y_fp = vec![
        SCALE - tiny, // dominant group ≈ 99.9999%
        tiny,         // tiny group ≈ 0.0001% — used to collapse to 0
    ];
    let p = vec![1000u64, 1];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 1001, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    assert!(
        !result[1].is_zero(),
        "tiny FI group must receive a non-zero fraction, got {}",
        result[1]
    );
}

/// When mass of Y is concentrated on the high end, `beta_num = f/fmax - E[Y]`
/// is negative and the algorithm must still produce a well-defined, bounded
/// distribution. Pre- the `/1_000_000` rounding could obliterate the
/// signed contribution for the lower-Y group.
#[test]
fn test_negative_beta_branch_produces_bounded_distribution() {
    let y_fp = vec![
        SCALE / U256::from(100u64),
        SCALE * U256::from(99u64) / U256::from(100u64),
    ]; // 1% / 99% split
    let p = vec![1u64, 1];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 2, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    let bound = F_MAX_FP * U256::from(2u64);
    for &f in &result {
        assert!(f <= bound, "fraction {f} exceeds LYSIS_LIMIT_MAX*2 bound");
    }
}

// ---------------------------------------------------------------------------
// Scale invariant: cost_amount_minor must be in 10^18-minor units, not 10^36
// ---------------------------------------------------------------------------

/// Regression test for the scale-mismatch bug in `lysis::runtime`. Both
/// `cost_of_gratis_minor` (an oracle VWAP at 10^18 scale) and `gratis_load`
/// (a token amount at 10^18 minor scale) are 18-decimal U256 values. Their
/// product lives in 10^36 and must be divided by SCALE once to land in
/// minor units. The contract is documented at
/// `crates/core/nod/src/schema.rs:6-7`:
///   `cost_amount_minor = cost_of_gratis_minor * gratis_load_minor / SCALE_1E18`
///
/// Pre-fix: `lysis::runtime` computed `cost_of_gratis_minor * gratis_load`
/// without the divisor, producing a value ~10^18× too large that was stored
/// on-chain and emitted to the `NodIssued` event. This was silent because
/// `settle_mine_payment` is a no-op today, but every nominal-scale consumer
/// (token URI, `nodData`, future settlement) was wrong.
#[test]
fn lysis_reads_repository_body_with_empty_legacy_evm_body_state() {
    use alloy_primitives::{address, U256};
    use outbe_common::WorldwideDay;
    use outbe_oracle::contract::OracleContract;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_tribute::TributeData;

    use crate::runtime::lysis;

    let wwd = WorldwideDay::new(20241220);
    const T_NOW: u64 = 1_700_000_000;
    let owner = address!("0x1111111111111111111111111111111111111111");
    // 100 COEN nominal, $0.5 oracle VWAP.
    let nominal = U256::in_units(100u64);
    let cost_of_gratis = U256::from(500_000_000_000_000_000u128);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    let bodies = TestBodyRepository::new();
    let result = StorageHandle::enter(&mut storage, |s| {
        // 1. Register COEN/0xUSD pair and seed its WorldwideDay VWAP. We
        //    write directly into the oracle schema (no real vote tally),
        //    because lysis only reads `get_worldwide_day_vwap_for_pair_id`.
        let mut oracle = OracleContract::new(s.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        // Wire ISO 840 → COEN/0xUSD so the runtime's ISO-keyed pair lookup resolves.
        let pair_hash = OracleContract::pair_hash("COEN", "0xUSD");
        oracle
            .settlement_iso_to_pair
            .write(&840u16, pair_hash)
            .unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        oracle
            .worldwide_day_vwap_pair_count
            .write(&wwd, 1u32)
            .unwrap();
        oracle
            .worldwide_day_vwap_pair_id
            .get_nested(&wwd)
            .write(&0u32, pair_id)
            .unwrap();
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&0u32, cost_of_gratis)
            .unwrap();

        // Seed compact lifecycle state plus the canonical direct-map commitment,
        // then materialize only the off-chain body. No legacy full EVM body or
        // body index is involved.
        let tribute = TributeData {
            tribute_id: entity_id(wwd, owner),
            owner,
            worldwide_day: wwd,
            issuance_amount_minor: U256::in_units(50u64),
            issuance_currency: 1,
            nominal_amount_minor: nominal,
            reference_currency: 840,
            exclude_from_intex_issuance: false,
            tribute_price_minor: U256::ZERO,
        };
        let mut tribute_contract = TributeContract::new(s.clone());
        tribute_contract.unseal_day(wwd).unwrap();
        bodies.issue(&mut tribute_contract, &tribute);

        // 3. Pick a gratis allocation that produces a positive gratis_load.
        //    Single-FI fast path returns `f_fp = LYSIS_LIMIT_MIN` (8%), so
        //    gratis_load = 100 * 0.08 = 8 COEN.
        let gratis_allocation = nominal / U256::from(10u64);
        let result = lysis(
            s.clone(),
            &bodies.tribute_reader,
            &bodies.nod_reader,
            wwd,
            T_NOW,
            gratis_allocation,
        )
        .unwrap();
        assert_eq!(result.nod_ids.len(), 1, "expected one NOD issued");
        result
    });

    // 4. Decode the canonical projection event and assert the documented scale invariant.
    let item = storage
        .get_events(NOD_ADDRESS)
        .iter()
        .find(|event| event.topics()[0] == INod::NodBodyStored::SIGNATURE_HASH)
        .map(decode_nod_body_event)
        .expect("NOD body event");
    assert_eq!(item.nod_id, result.nod_ids[0]);
    assert_eq!(item.reference_currency, 840);

    let expected = cost_of_gratis * item.gratis_load_minor / SCALE_1E18;
    assert_eq!(
        item.cost_amount_minor,
        expected,
        "cost_amount_minor must equal cost_of_gratis * gratis_load / SCALE_1E18; \
         pre-fix value (missing /SCALE) would be {}",
        cost_of_gratis * item.gratis_load_minor
    );

    let upper_bound = U256::in_units(1_000u64);
    assert!(
        item.cost_amount_minor <= upper_bound,
        "cost_amount_minor {} looks like a 10^36-scaled value; likely a scale-mismatch regression",
        item.cost_amount_minor
    );
}

/// 15 distinct-amount tributes, all bearing fidelity index 1. Sum is a clean
/// 1200 COEN so the percentage scenarios (5%/30%/32%) divide exactly with no
/// integer truncation in the deficit derivation — the assertions can use
/// strict equality rather than tolerance bands.
fn uniform_fi_one_population_15() -> (Vec<U256>, Vec<u16>, U256) {
    let nominal_amounts: Vec<U256> = (1u64..=15).map(|i| U256::in_units(10u64 * i)).collect();
    let tribute_fis = vec![1u16; 15];
    let total_interest: U256 = nominal_amounts
        .iter()
        .copied()
        .fold(U256::ZERO, |acc, v| acc + v);
    // Sanity: 10 * (1+2+...+15) = 1200 COEN.
    debug_assert_eq!(total_interest, U256::in_units(1200u64));
    (nominal_amounts, tribute_fis, total_interest)
}

#[test]
fn test_compute_fi_fraction_map_single_fi_five_percent_allocation() {
    let (nominal_amounts, tribute_fis, total_interest) = uniform_fi_one_population_15();
    // 5% deficit — well below the historical 8% floor.
    let gratis_allocation = total_interest * U256::from(5u64) / U256::from(100u64);

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        gratis_allocation,
    )
    .unwrap();

    assert_eq!(map.len(), 1, "all FI=1 must collapse to one map entry");
    let expected = SCALE * U256::from(5u64) / U256::from(100u64); // 0.05 * 10^18
    assert_eq!(
        map.get(&1).copied(),
        Some(expected),
        "scarce-gratis fraction must equal the 5% deficit coefficient"
    );
    println!("deficit fraction map: {:?}", map);
}

#[test]
fn test_compute_fi_fraction_map_single_fi_thirty_percent_allocation() {
    let (nominal_amounts, tribute_fis, total_interest) = uniform_fi_one_population_15();
    // 30% deficit — well above the historical 8%/16% range; the new logic
    // must not silently cap the fraction at 16%.
    let gratis_allocation = total_interest * U256::from(30u64) / U256::from(100u64);

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        gratis_allocation,
    )
    .unwrap();

    assert_eq!(map.len(), 1);
    let expected = SCALE * U256::from(30u64) / U256::from(100u64); // 0.30 * 10^18
    assert_eq!(
        map.get(&1).copied(),
        Some(expected),
        "abundant-gratis fraction must track the 30% deficit, not pin at 16%"
    );
}

#[test]
fn test_compute_fi_fraction_map_single_fi_thirtytwo_percent_allocation() {
    let (nominal_amounts, tribute_fis, total_interest) = uniform_fi_one_population_15();
    // 32% — matches the canonical metadosis symbolic rate (D1 in
    // metadosis-lysis-discrepancies.md). The fraction must reach 0.32, exactly.
    let gratis_allocation = total_interest * U256::from(32u64) / U256::from(100u64);

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        gratis_allocation,
    )
    .unwrap();

    assert_eq!(map.len(), 1);
    let expected = SCALE * U256::from(32u64) / U256::from(100u64); // 0.32 * 10^18
    assert_eq!(
        map.get(&1).copied(),
        Some(expected),
        "32% gratis allocation must produce a 32% fraction"
    );
}

/// Multi-FI scenario: 100 distinct-nominal tributes spread across 15 fidelity
/// indices, 32% gratis allocation.
#[test]
fn test_compute_fi_fraction_map_100_tributes_15_fis_thirtytwo_percent_allocation() {
    use std::collections::BTreeMap;

    // Distinct nominals 1..=100 COEN. Sum = 5050 COEN; 32% = 1616 COEN exactly.
    let nominal_amounts: Vec<U256> = (1u64..=100).map(U256::in_units).collect();
    // Round-robin FI assignment over 1..=15: FIs 1..=10 each get 7 tributes,
    // FIs 11..=15 each get 6 — covers every bucket with uneven population.
    let tribute_fis: Vec<u16> = (0u16..100).map(|i| (i % 15) + 1).collect();
    let total_interest: U256 = nominal_amounts
        .iter()
        .copied()
        .fold(U256::ZERO, |acc, v| acc + v);
    debug_assert_eq!(total_interest, U256::in_units(5050u64));

    let gratis_allocation = total_interest * U256::from(32u64) / U256::from(100u64);
    debug_assert_eq!(gratis_allocation, U256::in_units(1616u64));

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        gratis_allocation,
    )
    .unwrap();

    // 1. Every distinct FI must appear in the map.
    assert_eq!(
        map.len(),
        15,
        "every FI bucket present in input must receive a fraction"
    );
    for fi in 1u16..=15 {
        assert!(map.contains_key(&fi), "FI {fi} missing from fraction map");
    }

    // 2. Every fraction must be positive — the I256 pipeline must not collapse
    //    any group to zero, and the moment solver must not produce a negative
    //    that clamps to 0 (would starve a whole FI bucket).
    for (fi, frac) in &map {
        assert!(
            !frac.is_zero(),
            "FI {fi} got zero fraction: algorithm collapsed a group (got {frac})"
        );
    }

    // 3. Algorithm-level budget invariant. Reconstruct the y_fp vector exactly
    //    as the runtime does (BTreeMap-ordered group share with the truncation
    //    delta absorbed into the last entry) and assert the normalized
    //    `Σ(f_g · y_fp_g)/SCALE ≤ f_fp` post-condition. This is the
    //    `assert_weighted_within_target` invariant lifted to multi-FI inputs.
    let mut group_interest: BTreeMap<u16, U256> = BTreeMap::new();
    for (i, &fi) in tribute_fis.iter().enumerate() {
        *group_interest.entry(fi).or_insert(U256::ZERO) += nominal_amounts[i];
    }
    let mut y_fp: Vec<U256> = group_interest
        .values()
        .map(|gi| *gi * SCALE_1E18 / total_interest)
        .collect();
    let y_sum: U256 = y_fp.iter().copied().sum();
    if let Some(last) = y_fp.last_mut() {
        if y_sum < SCALE {
            *last += SCALE - y_sum;
        }
    }
    let weighted: U256 = group_interest
        .keys()
        .zip(y_fp.iter())
        .map(|(fi, y)| {
            let f = map.get(fi).copied().unwrap_or(U256::ZERO);
            f * *y / SCALE
        })
        .sum();
    let f_fp = SCALE * U256::from(32u64) / U256::from(100u64); // 0.32 * 10^18
    assert!(
        weighted <= f_fp,
        "weighted Σ(f·y_fp)/SCALE = {weighted} exceeds f_fp {f_fp} (32% budget violated)"
    );

    println!("100-tribute / 15-FI fraction map: {:?}", map);
    println!("weighted Σ(f·y_fp)/SCALE: {} (f_fp: {})", weighted, f_fp);
}

/// D3 regression (runtime path): when gratis is scarce (deficit < 8%), the per-FI
/// floor must adapt DOWN so the whole — small — allocation is loaded onto the
/// tribute. Under the previous degenerate `clamp(MIN, MAX/2)` the floor was pinned
/// to 8%, computing a gratis_load of 8% of nominal (> the 4% allocation), which
/// exceeds `remaining` and causes the NOD issuance to be SKIPPED entirely.
///
/// Reference behavior: outbe-cosmos `x/lysis/keeper/keeper.go` `x = min(8%, deficit)`.
#[test]
fn test_lysis_scarce_gratis_adapts_floor_below_eight_percent() {
    use alloy_primitives::{address, U256};
    use outbe_common::WorldwideDay;
    use outbe_oracle::contract::OracleContract;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_tribute::{TributeContract, TributeData};

    use crate::runtime::lysis;

    let wwd = WorldwideDay::new(20241221);
    const T_NOW: u64 = 1_700_000_000;
    let owner = address!("0x2222222222222222222222222222222222222222");
    let nominal = U256::in_units(100u64);
    let cost_of_gratis = U256::from(500_000_000_000_000_000u128);

    // Scarce: allocation is only 4% of nominal → deficit (4%) is BELOW the 8% floor.
    let gratis_allocation = nominal * U256::from(4u64) / U256::from(100u64);
    let eight_percent_load = nominal * U256::from(8u64) / U256::from(100u64);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    let bodies = TestBodyRepository::new();
    let result = StorageHandle::enter(&mut storage, |s| {
        let mut oracle = OracleContract::new(s.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        // Wire ISO 840 → COEN/0xUSD so the runtime's ISO-keyed pair lookup resolves.
        let pair_hash = OracleContract::pair_hash("COEN", "0xUSD");
        oracle
            .settlement_iso_to_pair
            .write(&840u16, pair_hash)
            .unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        oracle
            .worldwide_day_vwap_pair_count
            .write(&wwd, 1u32)
            .unwrap();
        oracle
            .worldwide_day_vwap_pair_id
            .get_nested(&wwd)
            .write(&0u32, pair_id)
            .unwrap();
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&0u32, cost_of_gratis)
            .unwrap();

        let mut tribute = TributeContract::new(s.clone());
        tribute.unseal_day(wwd).unwrap();
        bodies.issue(
            &mut tribute,
            &TributeData {
                tribute_id: entity_id(wwd, owner),
                owner,
                worldwide_day: wwd,
                issuance_amount_minor: U256::in_units(50u64),
                issuance_currency: 1,
                nominal_amount_minor: nominal,
                reference_currency: 840,
                exclude_from_intex_issuance: false,
                tribute_price_minor: U256::ZERO,
            },
        );

        let result = lysis(
            s.clone(),
            &bodies.tribute_reader,
            &bodies.nod_reader,
            wwd,
            T_NOW,
            gratis_allocation,
        )
        .unwrap();

        // With the fix, the floor adapts to 4% and the NOD is issued. The buggy
        // (pinned-8%) path would compute an 8% load > remaining and skip issuance.
        assert_eq!(
            result.nod_ids.len(),
            1,
            "scarce-gratis day must still issue the NOD (floor adapts to the 4% deficit)"
        );

        assert!(
            result.remaining_gratis.is_zero(),
            "the full scarce allocation must be consumed"
        );
        result
    });

    let item = storage
        .get_events(NOD_ADDRESS)
        .iter()
        .find(|event| event.topics()[0] == INod::NodBodyStored::SIGNATURE_HASH)
        .map(decode_nod_body_event)
        .expect("NOD body event");
    assert_eq!(item.nod_id, result.nod_ids[0]);
    assert_eq!(item.gratis_load_minor, gratis_allocation);
    assert!(item.gratis_load_minor < eight_percent_load);
}

// ---------------------------------------------------------------------
// Creator-reward: lysis records the per-owner contributor map
// ---------------------------------------------------------------------

#[test]
fn lysis_records_contributors_aggregated_by_owner() {
    const T_NOW: u64 = 1_700_000_000;
    let wwd = WorldwideDay::new(20260526);
    let cost_of_gratis = U256::from(500_000_000_000_000_000u128);
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    let bodies = TestBodyRepository::new();

    StorageHandle::enter(&mut storage, |storage| {
        // Oracle: register ISO 840 -> COEN/0xUSD and seed a day VWAP snapshot.
        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        let pair_hash = OracleContract::pair_hash("COEN", "0xUSD");
        oracle
            .settlement_iso_to_pair
            .write(&840u16, pair_hash)
            .unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        oracle
            .worldwide_day_vwap_pair_count
            .write(&wwd, 1u32)
            .unwrap();
        oracle
            .worldwide_day_vwap_pair_id
            .get_nested(&wwd)
            .write(&0u32, pair_id)
            .unwrap();
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&0u32, cost_of_gratis)
            .unwrap();

        // Distinct owners: lysis derives nod_id from (owner, day), so an owner
        // can have at most one processed tribute per day.
        let owner_a = gas_audit_address(1);
        let owner_b = gas_audit_address(2);
        let owner_c = gas_audit_address(3);

        let mut tribute = TributeContract::new(storage.clone());
        tribute.unseal_day(wwd).unwrap();
        bodies.issue(
            &mut tribute,
            &gas_audit_tribute(1, owner_a, wwd, U256::in_units(100u64)),
        );
        bodies.issue(
            &mut tribute,
            &gas_audit_tribute(2, owner_b, wwd, U256::in_units(200u64)),
        );
        bodies.issue(
            &mut tribute,
            &gas_audit_tribute(3, owner_c, wwd, U256::in_units(300u64)),
        );

        let total_nominal = U256::in_units(600u64);
        let gratis_allocation = total_nominal / U256::from(10u64);

        // The auction runs weeks after the tribute day; its date key — not the
        // wwd — is the series id the map must land under.
        const AUCTION_TS: u64 = 1_782_000_000; // 2026-06-21 UTC
        let result = crate::runtime::lysis(
            storage.clone(),
            &bodies.tribute_reader,
            &bodies.nod_reader,
            wwd,
            AUCTION_TS,
            gratis_allocation,
        )
        .expect("lysis must complete");
        assert_eq!(
            result.nod_ids.len(),
            3,
            "every tribute must be processed for this fixture"
        );

        // Contributors are sorted by address (a < b < c) and carry each
        // owner's nominal, under the auction series id (desis derivation).
        let series_id = outbe_primitives::time::timestamp_to_date_key(AUCTION_TS);
        assert_ne!(series_id, u32::from(wwd), "fixture must exercise the lag");
        assert_eq!(
            outbe_intex::api::read_contributors(&storage, series_id).unwrap(),
            vec![
                (owner_a, U256::in_units(100u64)),
                (owner_b, U256::in_units(200u64)),
                (owner_c, U256::in_units(300u64)),
            ]
        );
        assert_eq!(
            outbe_intex::api::contributor_total(&storage, series_id).unwrap(),
            U256::in_units(600u64)
        );
        assert_eq!(
            outbe_intex::api::contributor_count(&storage, u32::from(wwd)).unwrap(),
            0,
            "map must not be keyed by the tribute day"
        );
    });
}
