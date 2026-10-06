//! State-level tests: storage slot layout parity with genesis seeding and OCOMP opening plans.

use alloy_primitives::{Address, U256};

use crate::schema::OracleContract;

use super::common::*;

/// Probes the macro-assigned slot for `reference_currencies` so that
/// `scripts/seed_genesis.py` can mirror the layout. The StorageVec stores
/// its length at the base slot. The test pushes two values. Then it scans
/// slots 0..128 linearly for the length cell (== 2) to recover the slot.
#[test]
fn reference_currencies_occupies_slot_55() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;

    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        oracle.reference_currencies.push(840).unwrap();
        oracle.reference_currencies.push(978).unwrap();

        // Linear scan to find the slot whose word equals 2 (the length).
        let mut found: Option<u64> = None;
        for slot in 0u64..128 {
            let word = storage.sload(ORACLE_ADDRESS, U256::from(slot)).unwrap();
            if word == U256::from(2u64) {
                found = Some(slot);
                break;
            }
        }
        let slot = found.expect("could not locate reference_currencies length slot");

        println!("reference_currencies base slot = {slot}");

        // Verify the data lives at keccak256(slot) + 0 / + 1.
        use alloy_primitives::keccak256;
        let data_start = U256::from_be_bytes(keccak256(U256::from(slot).to_be_bytes::<32>()).0);
        assert_eq!(
            storage.sload(ORACLE_ADDRESS, data_start).unwrap(),
            U256::from(840u64),
            "data[0] mismatch at slot {slot}"
        );
        assert_eq!(
            storage
                .sload(ORACLE_ADDRESS, data_start + U256::from(1u64))
                .unwrap(),
            U256::from(978u64),
            "data[1] mismatch at slot {slot}"
        );

        // Hard-coded slot that scripts/seed_genesis.py uses. Keep in sync.
        assert_eq!(
            slot, 55,
            "macro-assigned reference_currencies slot changed; update scripts/seed_genesis.py"
        );
    });
}

/// Pins `pair_by_index` to slot 43, and its quote word to that slot plus one.
///
/// It sits immediately after the retired settlement hole (40-42). Thus only the
/// `#[slot(43)]` anchor keeps it in place. Without the anchor, the running slot
/// counter of the macro would slide it into the hole and silently repoint
/// `scripts/seed_genesis.py`. The quote word lives at `+1` inside the hashed
/// namespace of the key, not in a declaration slot. Thus the test asserts the
/// quote word directly and does not find it by sweeping slot numbers.
#[test]
fn pair_by_index_occupies_slot_43_as_a_two_word_value() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;
    use outbe_primitives::storage::types::StorageKey;

    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        oracle
            .pair_by_index
            .write_pair(&1u32, pair_key(USDT, USDC))
            .unwrap();

        let entry_slot = 1u32.mapping_slot(U256::from(43u64));
        let word =
            |slot: U256| Address::from_word(storage.sload(ORACLE_ADDRESS, slot).unwrap().into());

        assert_eq!(
            word(entry_slot),
            USDT,
            "macro-assigned pair_by_index slot changed; the #[slot(43)] anchor \
             after the retired settlement hole did not hold"
        );
        assert_eq!(
            word(entry_slot + U256::from(1)),
            USDC,
            "the quote word is not at the entry slot + 1; update \
             scripts/seed_genesis.py::set_mapping_pair"
        );
    });
}

/// Pins every base slot the frozen OCOMP V1 opening plan (`openings.rs`) and
/// `scripts/seed_genesis.py` hardcode. The test writes each field through the
/// typed schema and reads it back at the raw slot that those consumers derive.
/// Thus any field reorder or mis-placed `#[slot(N)]` pin fails here and does
/// not silently corrupt a genesis seed or an opening proof.
///
/// Slots 41 and 46 are retired holes. They stay in the V1 plan (whose codec
/// descriptor is hashed into the protocol bundle), but they have no live
/// writer. Thus they must read as zero after a full genesis init.
#[test]
fn ocomp_opening_plan_slots_match_the_schema_layout() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;
    use outbe_primitives::storage::types::StorageKey;
    use outbe_primitives::storage::StorageHandle;

    fn assert_mapping_slot<K: StorageKey>(
        storage: &StorageHandle<'_>,
        key: K,
        base: U256,
        expected: U256,
        field: &str,
    ) {
        assert_eq!(
            storage
                .sload(ORACLE_ADDRESS, key.mapping_slot(base))
                .unwrap(),
            expected,
            "{field} is not at base slot {base}; openings.rs and \
             scripts/seed_genesis.py hardcode it"
        );
    }

    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let wwd = outbe_primitives::time::WorldwideDay::from_timestamp(ATOMIC_DAY_START);
        let iso: u16 = 840;
        // The exact key `openings.rs` derives for a reference ISO. Writing it
        // through the typed schema and reading it back at the raw slot pins the
        // 40-byte `mapping_slot` derivation as well as the slot number.
        let pair = AddressPair::new_coen_to(iso);

        oracle.pair_to_index.write(&pair, 7).unwrap();
        oracle.scurve_count.write(3).unwrap();
        oracle
            .scurve_pair
            .write_pair(&0u32, pair_key(COEN, usd()))
            .unwrap();
        oracle.scurve_peak_day.write(&0u32, 111).unwrap();
        oracle
            .scurve_peak_price
            .write(&0u32, U256::from(222u64))
            .unwrap();
        oracle.scurve_oldest_idx.write(1).unwrap();
        oracle.reference_currencies.push(iso).unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        // Both VWAP value columns use the registry index that the pair got above
        // as their key. Thus the test pins the raw slot derivation against that index.
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&7u32, U256::from(333u64))
            .unwrap();
        oracle
            .record_utc_day_vwap(20260302u32, 7u32, U256::from(444u64))
            .unwrap();

        // Direct (non-mapping) slots.
        for (slot, expected, field) in [(34u64, 3u64, "scurve_count"), (38, 1, "scurve_oldest_idx")]
        {
            assert_eq!(
                storage.sload(ORACLE_ADDRESS, U256::from(slot)).unwrap(),
                U256::from(expected),
                "{field} is not at slot {slot}"
            );
        }

        let base = U256::from;
        assert_mapping_slot(&storage, pair, base(10), base(7), "pair_index");
        let as_word = |a: Address| U256::from_be_bytes(a.into_word().0);
        // A pair value spans two words: base at the entry slot, quote at +1.
        assert_mapping_slot(&storage, 0u32, base(35), as_word(COEN), "scurve_pair");
        assert_eq!(
            storage
                .sload(ORACLE_ADDRESS, 0u32.mapping_slot(base(35)) + base(1))
                .unwrap(),
            as_word(usd()),
            "scurve_pair quote word is not at the entry slot + 1"
        );
        assert_mapping_slot(&storage, 0u32, base(36), base(111), "scurve_peak_day");
        assert_mapping_slot(&storage, 0u32, base(37), base(222), "scurve_peak_price");
        // reference_currencies is a StorageVec: length at the base slot,
        // elements at keccak256(be32(base)) + index.
        assert_eq!(
            storage.sload(ORACLE_ADDRESS, base(55)).unwrap(),
            base(1),
            "reference_currencies length is not at slot 55"
        );
        let reference_data =
            U256::from_be_bytes(alloy_primitives::keccak256(base(55).to_be_bytes::<32>()).0);
        assert_eq!(
            storage.sload(ORACLE_ADDRESS, reference_data).unwrap(),
            U256::from(iso),
            "reference_currencies[0] is not at keccak256(be32(55))"
        );
        assert_mapping_slot(
            &storage,
            wwd,
            base(47),
            base(1),
            "worldwide_day_vwap_exists",
        );
        // Nested maps: the outer key derives the inner map's base slot.
        assert_mapping_slot(
            &storage,
            7u32,
            wwd.mapping_slot(base(52)),
            base(333),
            "worldwide_day_vwap_value",
        );
        assert_mapping_slot(
            &storage,
            7u32,
            20260302u32.mapping_slot(base(58)),
            base(444),
            "utc_day_vwap_value",
        );
    });
}

#[test]
fn worldwide_day_partial_aggregates_occupy_slots_70_through_73() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;
    use outbe_primitives::storage::types::StorageKey;

    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        let day = 1_780_012_800u64;
        let markers = [11u64, 12, 13, 14];
        oracle
            .wwd_suffix_pv_sum
            .get_nested(&pair)
            .write(&day, U256::from(markers[0]))
            .unwrap();
        oracle
            .wwd_suffix_vol_sum
            .get_nested(&pair)
            .write(&day, U256::from(markers[1]))
            .unwrap();
        oracle
            .wwd_prefix_pv_sum
            .get_nested(&pair)
            .write(&day, U256::from(markers[2]))
            .unwrap();
        oracle
            .wwd_prefix_vol_sum
            .get_nested(&pair)
            .write(&day, U256::from(markers[3]))
            .unwrap();

        for (base, marker) in (70u64..=73).zip(markers) {
            let outer = pair.mapping_slot(U256::from(base));
            let slot = day.mapping_slot(outer);
            assert_eq!(
                storage.sload(ORACLE_ADDRESS, slot).unwrap(),
                U256::from(marker),
                "WWD partial aggregate moved from slot {base}"
            );
        }
    });
}

/// Every retired slot in the settlement range must stay empty. The frozen V1
/// plan still opens slots 41/42, so a resurrected writer would change what
/// that plan proves. Slots 40/45/46 must stay clear so the holes remain
/// reusable-free and the `#[slot(43)]` anchor keeps its meaning.
#[test]
fn retired_settlement_slots_stay_zero_after_genesis() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;
    use outbe_primitives::storage::types::StorageKey;

    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let config = crate::genesis::OracleGenesisConfig::default_config();
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        // Retired mappings, probed at the key genesis would have used.
        for base in [41u64, 42, 45, 46] {
            let slot = 840u16.mapping_slot(U256::from(base));
            assert_eq!(
                storage.sload(ORACLE_ADDRESS, slot).unwrap(),
                U256::ZERO,
                "retired slot {base} was written; it has no live writer"
            );
            // settlement_index_to_iso was keyed by a u32 index, not the ISO.
            let index_slot = 0u32.mapping_slot(U256::from(base));
            assert_eq!(
                storage.sload(ORACLE_ADDRESS, index_slot).unwrap(),
                U256::ZERO,
                "retired slot {base} was written at index 0"
            );
        }

        // Retired direct slot (former settlement_count).
        assert_eq!(
            storage.sload(ORACLE_ADDRESS, U256::from(40u64)).unwrap(),
            U256::ZERO,
            "retired slot 40 was written; it has no live writer"
        );
    });
}

#[test]
fn slot_60_is_retired_and_policy_registry_occupies_slots_74_and_75() {
    use alloy_primitives::keccak256;
    use outbe_primitives::addresses::ORACLE_ADDRESS;

    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let iso: u16 = 840;
        let marker = U256::from(0x00AB_CDEFu64);
        oracle.policy_rate_currencies.push(iso).unwrap();
        oracle.policy_rate.write(&iso, marker).unwrap();

        assert_eq!(
            storage.sload(ORACLE_ADDRESS, U256::from(60)).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            storage.sload(ORACLE_ADDRESS, U256::from(74)).unwrap(),
            U256::from(1)
        );

        let element_slot = U256::from_be_bytes(keccak256(U256::from(74).to_be_bytes::<32>()).0);
        assert_eq!(
            storage.sload(ORACLE_ADDRESS, element_slot).unwrap(),
            U256::from(iso)
        );

        let mut buf = [0u8; 64];
        buf[30..32].copy_from_slice(&iso.to_be_bytes());
        buf[32..64].copy_from_slice(&U256::from(75).to_be_bytes::<32>());
        let rate_slot = U256::from_be_bytes(keccak256(buf).0);
        assert_eq!(storage.sload(ORACLE_ADDRESS, rate_slot).unwrap(), marker);
    });
}

#[test]
fn genesis_seeds_the_usd_policy_rate() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(
            &mut oracle,
            &crate::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();
        assert_eq!(oracle.get_policy_rate(840).unwrap(), U256::from(36_300u64));
    });
}
