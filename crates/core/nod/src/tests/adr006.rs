use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::WwdEntityId;
use outbe_compressed_entities::{begin_block, ExecutionReaders, ExecutionScope};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::time::{
    date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key, WorldwideDay,
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    math::constants::REAL_ID_SHIFT,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use outbe_oracle::api::AddressPair;

use crate::{api, NodCertifiedGenerationProjection, NodContract, NodItemState};

/// Default block timestamp for qualification tests. Not UTC midnight, so its
/// last closed day is the previous calendar day.
const NOW: u64 = 1_752_534_000;

fn item(owner: Address) -> NodItemState {
    let worldwide_day = WorldwideDay::new(20_260_715);
    crate::test_support::item(
        crate::test_support::NodItemFixture {
            is_settled: false,
            nod_id: crate::identity::generate_nod_id(owner, worldwide_day).unwrap(),
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day,
            league_id: 4,
            bucket_key: crate::identity::bucket_key(worldwide_day, U256::from(5), 978),
            issuance_currency: 840,
            reference_currency: 978,
            // Midnight of the last UTC day closed at `NOW`, so a bucket issued in
            // these fixtures can qualify on that day's VWAP.
            issued_at: date_key_to_utc_timestamp(previous_date_key(timestamp_to_date_key(NOW))),
        },
        U256::from(5),
    )
}

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        outbe_primitives::call_bins::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn reverted_issuance_rolls_back_overlay_compact_state_and_events() {
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let body = item(Address::repeat_byte(0x66));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::default();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage).expect("CE genesis fixture");
        begin_block(storage.clone(), &scope).unwrap();
        let outcome: Result<()> = storage.with_checkpoint(|| {
            api::add_nod(
                &storage,
                outbe_compressed_entities::ExecutionReaders {
                    scope: &scope,
                    parent: &parent,
                },
                &body,
                U256::from(5),
                crate::test_support::sealed_terms(&storage, &body, U256::from(5)),
            )?;
            assert!(api::get_item(&storage, &scope, &parent, body.nod_id)?.is_some());
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(outcome.is_err());
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert!(api::get_item(&storage, &scope, &parent, body.nod_id)
            .unwrap()
            .is_none());
    });
}

#[test]
fn nod_identity_and_abi_boundary_preserve_exact_32_bytes() {
    let body = item(Address::repeat_byte(0x33));
    let encoded = body.nod_id.to_string();
    assert_eq!(
        crate::identity::parse_nod_id(&encoded).unwrap(),
        body.nod_id
    );
    assert!(crate::identity::parse_nod_id(&encoded[..62]).is_err());

    // The ABI carries the identity as one word, so a wrong-width id is no
    // longer representable. The round trip through `uint256` is total. The old
    // "invalid bytes length" revert has no input that can reach it.
    let word = body.nod_id.to_u256();
    assert_eq!(WwdEntityId::from(word), body.nod_id);
    assert_eq!(word.to_be_bytes::<32>(), body.nod_id.0 .0);
}

#[test]
fn membership_changes_preserve_bucket_body_and_commitment_until_last_removal() {
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let first = item(Address::repeat_byte(0x71));
    let second = item(Address::repeat_byte(0x72));
    let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, first.bucket_key);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::default();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage).expect("CE genesis fixture");
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &first,
            U256::from(5),
            crate::test_support::sealed_terms(&storage, &first, U256::from(5)),
        )
        .unwrap();
        let original = NodContract::new(storage.clone())
            .get_bucket_verified(&scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &second,
            U256::from(5),
            crate::test_support::sealed_terms(&storage, &second, U256::from(5)),
        )
        .unwrap();
        let after_issue = NodContract::new(storage.clone())
            .get_bucket_verified(&scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            original, after_issue,
            "adding another NOD must not rewrite the shared bucket commitment"
        );
        assert_eq!(original.stored_body(), after_issue.stored_body());
        assert_eq!(
            NodContract::new(storage.clone())
                .bucket_nod_count
                .read(&first.bucket_key)
                .unwrap(),
            2
        );

        let loaded_item = api::load_item(&storage, &scope, &parent, first.nod_id)
            .unwrap()
            .unwrap();
        let bucket = api::load_bucket(&storage, &scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        api::remove_nod(&storage, &scope, loaded_item, bucket).unwrap();
        let after_removal = NodContract::new(storage.clone())
            .get_bucket_verified(&scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        assert_eq!(original, after_removal);
        assert_eq!(original.stored_body(), after_removal.stored_body());
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.bucket_nod_count.read(&first.bucket_key).unwrap(), 1);
        assert_eq!(
            nod.bucket_nods
                .read(&crate::index_keys::bucket_nod_key(first.bucket_key, 0))
                .unwrap(),
            second.nod_id
        );
        assert_eq!(nod.bucket_nod_index.read(&second.nod_id).unwrap(), 0);

        let loaded_item = api::load_item(&storage, &scope, &parent, second.nod_id)
            .unwrap()
            .unwrap();
        let bucket = api::load_bucket(&storage, &scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        api::remove_nod(&storage, &scope, loaded_item, bucket).unwrap();
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.bucket_nod_count.read(&first.bucket_key).unwrap(), 0);
        assert_eq!(nod.total_supply().unwrap(), 0);
        assert!(api::get_bucket(&storage, &scope, &parent, bucket_id)
            .unwrap()
            .is_none());
    });
}

#[test]
fn materialization_fifo_slots_match_the_genesis_seeder() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        assert_eq!(
            nod.ocomp_materialization_head_sequence.slot(),
            U256::from(13)
        );
        assert_eq!(
            nod.ocomp_materialization_tail_sequence.slot(),
            U256::from(14)
        );
    });
}

/// `openings` and the oracle-opening codec both name the Oracle's slots; both must be the schema's.
#[test]
fn entry_price_openings_follow_the_oracle_schema() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;

    let day = 20_260_725;
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        // GBP is registered without a VWAP for the day; EUR is not registered at all.
        let gbp = outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(826))
            .unwrap();
        let usd = outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(840))
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .record_utc_day_vwap(day, usd, U256::from(320_000))
            .unwrap();
        let isos = [826, 840, 978];
        let pair_slots = crate::openings::pair_index_slots(&isos).unwrap();
        let indices = pair_slots
            .iter()
            .map(|slot| {
                storage
                    .sload(ORACLE_ADDRESS, U256::from_be_bytes(slot.0))
                    .unwrap()
                    .to::<u32>()
            })
            .collect::<Vec<_>>();
        assert_eq!(indices, vec![gbp, usd, 0]);
        let opened = crate::openings::entry_price_slots(day, &isos, &indices)
            .unwrap()
            .into_iter()
            .map(|slot| {
                let value = storage
                    .sload(ORACLE_ADDRESS, U256::from_be_bytes(slot.0))
                    .unwrap();
                (slot, value)
            })
            .collect::<Vec<_>>();
        let prices = crate::openings::evaluate_entry_prices(day, &isos, &opened).unwrap();
        assert_eq!(
            prices,
            std::collections::BTreeMap::from([(840, U256::from(320_000))])
        );
        for iso in isos {
            assert_eq!(
                prices.get(&iso).copied(),
                outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), day, iso).unwrap(),
                "the opening prices {iso} like the runtime Oracle read"
            );
        }
        assert!(crate::openings::evaluate_entry_prices(day + 1, &isos, &opened).is_err());
        let mut reordered = opened.clone();
        reordered.swap(0, 1);
        assert!(crate::openings::evaluate_entry_prices(day, &isos, &reordered).is_err());
        let mut forged_index = opened.clone();
        forged_index[2].1 = U256::from(usd);
        assert!(crate::openings::evaluate_entry_prices(day, &isos, &forged_index).is_err());
        let mut wide_index = opened.clone();
        wide_index[0].1 = U256::from(u64::from(u32::MAX) + 1);
        assert!(crate::openings::evaluate_entry_prices(day, &isos, &wide_index).is_err());
        assert!(crate::openings::evaluate_entry_prices(day, &isos, &opened[..3]).is_err());
        assert!(crate::openings::pair_index_slots(&[978]).is_err());
        assert!(crate::openings::pair_index_slots(&[840, 840]).is_err());
        assert!(crate::openings::pair_index_slots(&[0, 840]).is_err());
        assert!(
            outbe_ocomp_protocol::registry::ORACLE_OPENING_CODEC_DESCRIPTOR
                .contains("oracle_utc_day_vwap_slots_v1(pair_index=10,vwap=58,")
        );
    });
}

#[test]
fn entry_price_day_is_the_last_closed_utc_day() {
    let new_year = date_key_to_utc_timestamp(20_260_101);
    assert_eq!(crate::openings::entry_price_day(new_year), 20_251_231);
    assert_eq!(crate::openings::entry_price_day(new_year - 1), 20_251_230);
}

#[test]
fn member_count_overflow_and_underflow_roll_back_nod_mutations() {
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let first = item(Address::repeat_byte(0x73));
    let second = item(Address::repeat_byte(0x74));
    let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, first.bucket_key);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::default();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage).expect("CE genesis fixture");
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &first,
            U256::from(5),
            crate::test_support::sealed_terms(&storage, &first, U256::from(5)),
        )
        .unwrap();
        let nod = NodContract::new(storage.clone());
        let original = nod
            .get_bucket_verified(&scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        nod.bucket_nod_count
            .write(&first.bucket_key, u32::MAX)
            .unwrap();
        let error = api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &second,
            U256::from(5),
            crate::test_support::sealed_terms(&storage, &second, U256::from(5)),
        )
        .unwrap_err();
        assert!(
            matches!(error, PrecompileError::Revert(message) if message.contains("member index overflow"))
        );
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(
            nod.bucket_nod_count.read(&first.bucket_key).unwrap(),
            u32::MAX
        );
        assert!(api::get_item(&storage, &scope, &parent, second.nod_id)
            .unwrap()
            .is_none());

        nod.bucket_nod_count.write(&first.bucket_key, 0).unwrap();
        let loaded = api::load_item(&storage, &scope, &parent, first.nod_id)
            .unwrap()
            .unwrap();
        let bucket = api::load_bucket(&storage, &scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        let error = api::remove_nod(&storage, &scope, loaded, bucket).unwrap_err();
        assert!(
            matches!(error, PrecompileError::Revert(message) if message.contains("member count underflow"))
        );
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert!(api::get_item(&storage, &scope, &parent, first.nod_id)
            .unwrap()
            .is_some());
        assert_eq!(
            nod.get_bucket_verified(&scope, &parent, bucket_id)
                .unwrap()
                .unwrap(),
            original
        );
    });
}

#[test]
fn nod_error_and_calculation_amount_never_return_fatal() {
    let domain_error: PrecompileError = crate::errors::NodError::NonNodItemPayload.into();
    assert!(matches!(domain_error, PrecompileError::Revert(_)));

    let mut corrupted = item(Address::repeat_byte(0x75));
    crate::test_support::uninstall();
    assert!(matches!(
        api::calculation_amount(&corrupted),
        Err(PrecompileError::EnclaveUnavailable(_))
    ));

    crate::test_support::install();
    corrupted.encrypted.encrypted_gratis_amount.clear();
    assert!(matches!(
        api::calculation_amount(&corrupted),
        Err(PrecompileError::Revert(_))
    ));
}

/// Slot assignment is dense in `order` sequence. Inserting a field rather than
/// appending one silently reassigns the meaning of every slot after it,
/// including the two slots that the genesis alloc seeds. New fields must append.
#[test]
fn nod_contract_slot_layout_is_pinned() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        assert_eq!(nod.total_supply.slot(), U256::ZERO);
        assert_eq!(nod.bucket_worldwide_day.base_slot(), U256::from(1));
        assert_eq!(nod.ocomp_target_generation.base_slot(), U256::from(2));
        assert_eq!(
            nod.ocomp_materialization_attempt_count.slot(),
            U256::from(17)
        );
        assert_eq!(nod.bucket_nod_count.base_slot(), U256::from(18));
        assert_eq!(nod.bucket_nods.base_slot(), U256::from(19));
        assert_eq!(nod.bucket_nod_index.base_slot(), U256::from(20));
        assert_eq!(nod.bucket_call_price_minor.base_slot(), U256::from(21));
        assert_eq!(nod.bucket_currency.base_slot(), U256::from(22));
        assert_eq!(nod.bucket_called_at.base_slot(), U256::from(23));
        assert_eq!(
            nod.ocomp_materialization_protocol_bundle_hash.base_slot(),
            U256::from(24)
        );
        assert_eq!(nod.bucket_call_rate.base_slot(), U256::from(25));
        assert_eq!(nod.bucket_call_window_seconds.base_slot(), U256::from(26));
        assert_eq!(
            nod.bucket_call_threshold_seconds.base_slot(),
            U256::from(27)
        );
        assert_eq!(
            nod.bucket_call_notice_period_seconds.base_slot(),
            U256::from(28)
        );
        assert_eq!(nod.max_call_window_seconds.base_slot(), U256::from(29));
        assert_eq!(nod.bucket_issued_at.base_slot(), U256::from(30));
        assert_eq!(nod.call_sweep_day.slot(), U256::from(31));
        assert_eq!(nod.call_pending_day.slot(), U256::from(32));
        assert_eq!(nod.call_bin_tree_root.base_slot(), U256::from(33));
        assert_eq!(nod.call_bin_tree_mid.base_slot(), U256::from(34));
        assert_eq!(nod.call_bin_tree_leaf.base_slot(), U256::from(35));
        assert_eq!(nod.call_bin_count.base_slot(), U256::from(36));
        assert_eq!(nod.call_bin_buckets.base_slot(), U256::from(37));
        assert_eq!(nod.call_bucket_slot.base_slot(), U256::from(38));
        assert_eq!(nod.expiry_tree_root.slot(), U256::from(39));
        assert_eq!(nod.expiry_tree_leaf.base_slot(), U256::from(41));
        assert_eq!(nod.call_currency_cursor.slot(), U256::from(42));
        assert_eq!(nod.call_bin_cursor.base_slot(), U256::from(43));
        assert_eq!(nod.config_profile.slot(), U256::from(44));
        assert_eq!(nod.expiry_bucket_len.base_slot(), U256::from(45));
        assert_eq!(nod.expiry_cursor.slot(), U256::from(51));
    });
}

#[test]
fn certified_generation_is_available_through_the_public_nod_abi() {
    let worldwide_day = WorldwideDay::new(20_260_726);
    let generation = NodCertifiedGenerationProjection {
        worldwide_day,
        generation: 9,
        job_id: B256::repeat_byte(0x44),
        protocol_bundle_hash: B256::repeat_byte(0x5b),
        program_semantics_hash: B256::repeat_byte(0x55),
        nod_root: B256::repeat_byte(0x11),
        bucket_root: B256::repeat_byte(0x22),
        output_manifest_root: B256::repeat_byte(0x33),
        tribute_count: 257,
        nod_count: 257,
        bucket_count: 13,
        nod_amount_total: U256::from(50_000),
        lysis_allocation_minor: U256::from(7_000),
        nod_gratis_load_max: U256::from(1_000),
        issued_at: 1_753_488_000,
        next_nod_ordinal: 129,
        last_progress_height: 4_096,
    };
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let scope = ExecutionScope::default();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage.clone());
        nod.ocomp_target_generation
            .write(&worldwide_day, generation.generation)
            .unwrap();
        nod.ocomp_namespace_root
            .write(&worldwide_day, generation.nod_root)
            .unwrap();
        nod.ocomp_bucket_root
            .write(&worldwide_day, generation.bucket_root)
            .unwrap();
        nod.ocomp_output_manifest_root
            .write(&worldwide_day, generation.output_manifest_root)
            .unwrap();
        nod.ocomp_generation_metadata
            .write(&worldwide_day, generation.metadata_word())
            .unwrap();
        nod.ocomp_nod_amount_total
            .write(&worldwide_day, generation.nod_amount_total)
            .unwrap();
        nod.ocomp_lysis_allocation_minor
            .write(&worldwide_day, generation.lysis_allocation_minor)
            .unwrap();
        nod.ocomp_nod_gratis_load_max
            .write(&worldwide_day, generation.nod_gratis_load_max)
            .unwrap();
        nod.ocomp_materialization_job_id
            .write(&worldwide_day, generation.job_id)
            .unwrap();
        nod.ocomp_materialization_protocol_bundle_hash
            .write(&worldwide_day, generation.protocol_bundle_hash)
            .unwrap();
        nod.ocomp_materialization_program_semantics_hash
            .write(&worldwide_day, generation.program_semantics_hash)
            .unwrap();
        nod.ocomp_materialization_next_nod_ordinal
            .write(&worldwide_day, generation.next_nod_ordinal)
            .unwrap();
        nod.ocomp_materialization_last_progress_height
            .write(&worldwide_day, generation.last_progress_height)
            .unwrap();

        let call = crate::precompile::INod::certifiedGenerationCall {
            worldwideDay: worldwide_day.into(),
        }
        .abi_encode();
        let output = crate::precompile::dispatch(
            storage.clone(),
            ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &call,
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let actual =
            crate::precompile::INod::certifiedGenerationCall::abi_decode_returns(&output).unwrap();

        assert!(actual.exists);
        assert_eq!(actual.worldwideDay, worldwide_day.value());
        assert_eq!(actual.generation, generation.generation);
        assert_eq!(actual.nodRoot, generation.nod_root);
        assert_eq!(actual.bucketRoot, generation.bucket_root);
        assert_eq!(actual.outputManifestRoot, generation.output_manifest_root);
        assert_eq!(actual.tributeCount, generation.tribute_count);
        assert_eq!(actual.nodCount, generation.nod_count);
        assert_eq!(actual.bucketCount, generation.bucket_count);
        assert_eq!(actual.totalSettlementCostMinor, generation.nod_amount_total);
        assert_eq!(
            actual.lysisAllocationMinor,
            generation.lysis_allocation_minor
        );
        assert_eq!(actual.issuedAt, generation.issued_at);
    });
}

#[test]
fn absent_certified_generation_has_an_explicit_public_abi_result() {
    let worldwide_day = WorldwideDay::new(20_260_727);
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let scope = ExecutionScope::default();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let call = crate::precompile::INod::certifiedGenerationCall {
            worldwideDay: worldwide_day.into(),
        }
        .abi_encode();
        let output = crate::precompile::dispatch(
            storage,
            ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &call,
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let actual =
            crate::precompile::INod::certifiedGenerationCall::abi_decode_returns(&output).unwrap();

        assert!(!actual.exists);
        assert_eq!(actual.worldwideDay, worldwide_day.value());
        assert_eq!(actual.generation, 0);
        assert_eq!(actual.nodRoot, B256::ZERO);
        assert_eq!(actual.bucketRoot, B256::ZERO);
        assert_eq!(actual.outputManifestRoot, B256::ZERO);
        assert_eq!(actual.tributeCount, 0);
        assert_eq!(actual.nodCount, 0);
        assert_eq!(actual.bucketCount, 0);
        assert_eq!(actual.totalSettlementCostMinor, U256::ZERO);
        assert_eq!(actual.lysisAllocationMinor, U256::ZERO);
        assert_eq!(actual.issuedAt, 0);
    });
}

#[test]
fn the_certified_bundle_survives_a_read_and_leaves_nothing_behind_when_cleared() {
    let worldwide_day = WorldwideDay::new(20_260_726);
    let bundle = B256::repeat_byte(0x5b);
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage.clone());
        nod.ocomp_target_generation
            .write(&worldwide_day, 9)
            .unwrap();
        nod.ocomp_materialization_job_id
            .write(&worldwide_day, B256::repeat_byte(0x44))
            .unwrap();
        nod.ocomp_materialization_protocol_bundle_hash
            .write(&worldwide_day, bundle)
            .unwrap();
        nod.ocomp_materialization_program_semantics_hash
            .write(&worldwide_day, B256::repeat_byte(0x55))
            .unwrap();
        nod.ocomp_namespace_root
            .write(&worldwide_day, B256::repeat_byte(0x11))
            .unwrap();
        nod.ocomp_bucket_root
            .write(&worldwide_day, B256::repeat_byte(0x22))
            .unwrap();
        nod.ocomp_output_manifest_root
            .write(&worldwide_day, B256::repeat_byte(0x33))
            .unwrap();
        let shape = NodCertifiedGenerationProjection {
            worldwide_day,
            generation: 9,
            job_id: B256::repeat_byte(0x44),
            protocol_bundle_hash: bundle,
            program_semantics_hash: B256::repeat_byte(0x55),
            nod_root: B256::repeat_byte(0x11),
            bucket_root: B256::repeat_byte(0x22),
            output_manifest_root: B256::repeat_byte(0x33),
            tribute_count: 7,
            nod_count: 7,
            bucket_count: 2,
            nod_amount_total: U256::from(50_000),
            lysis_allocation_minor: U256::from(7_000),
            nod_gratis_load_max: U256::from(1_000),
            issued_at: 1_753_488_000,
            next_nod_ordinal: 0,
            last_progress_height: 4_096,
        };
        nod.ocomp_generation_metadata
            .write(&worldwide_day, shape.metadata_word())
            .unwrap();
        nod.ocomp_nod_amount_total
            .write(&worldwide_day, shape.nod_amount_total)
            .unwrap();
        nod.ocomp_lysis_allocation_minor
            .write(&worldwide_day, shape.lysis_allocation_minor)
            .unwrap();
        nod.ocomp_nod_gratis_load_max
            .write(&worldwide_day, shape.nod_gratis_load_max)
            .unwrap();
        nod.ocomp_materialization_last_progress_height
            .write(&worldwide_day, shape.last_progress_height)
            .unwrap();

        let read = nod
            .ocomp_certified_generation(worldwide_day)
            .unwrap()
            .expect("a generation with a non-zero number is present");
        assert_eq!(
            read.protocol_bundle_hash, bundle,
            "materialization reads the bundle from the chain, so it has to come back"
        );
        assert_eq!(read.nod_gratis_load_max, shape.nod_gratis_load_max);

        nod.clear_ocomp_certified_generation(worldwide_day).unwrap();
        assert!(
            nod.ocomp_certified_generation(worldwide_day)
                .unwrap()
                .is_none(),
            "clearing has to wipe the bundle too, or the day reads as residual state"
        );
    });
}
