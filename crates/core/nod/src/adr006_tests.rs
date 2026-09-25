use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::WwdEntityId;
use outbe_compressed_entities::{begin_block, ExecutionScope};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::time::{
    date_key_to_utc_timestamp, first_full_day, previous_date_key, timestamp_to_date_key,
    WorldwideDay,
};
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    error::{PrecompileError, Result},
    math::constants::REAL_ID_SHIFT,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn seed_compressed_entities_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();
}

use outbe_oracle::api::AddressPair;

use crate::{
    api, NodCertifiedGenerationProjection, NodContract, NodItemState, NodRepositoryReader,
};

/// Default block timestamp for qualification tests. Not UTC midnight, so its
/// last closed day is the previous calendar day.
const NOW: u64 = 1_752_534_000;

fn item(owner: Address) -> NodItemState {
    let worldwide_day = WorldwideDay::new(20_260_715);
    NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day,
        league_id: 4,
        floor_price_minor: U256::from(13),
        bucket_key: NodContract::bucket_key(worldwide_day, U256::from(13), 978),
        issuance_currency: 840,
        reference_currency: 978,
        // Midnight of the last UTC day closed at `NOW`, so a bucket issued in
        // these fixtures can qualify on that day's VWAP.
        issued_at: date_key_to_utc_timestamp(previous_date_key(timestamp_to_date_key(NOW))),
    }
}

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        NodContract::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn reverted_issuance_rolls_back_overlay_compact_state_and_events() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let body = item(Address::repeat_byte(0x66));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let outcome: Result<()> = storage.with_checkpoint(|| {
            api::add_nod(&storage, &scope, &parent, &body, U256::from(5))?;
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
    assert_eq!(NodContract::parse_nod_id(&encoded).unwrap(), body.nod_id);
    assert!(NodContract::parse_nod_id(&encoded[..62]).is_err());

    // The ABI carries the identity as one word, so a wrong-width id is no
    // longer representable: the round trip through `uint256` is total, and the
    // old "invalid bytes length" revert has no input that can reach it.
    let word = body.nod_id.to_u256();
    assert_eq!(WwdEntityId::from(word), body.nod_id);
    assert_eq!(word.to_be_bytes::<32>(), body.nod_id.0 .0);
}

#[test]
fn membership_changes_preserve_bucket_body_and_commitment_until_last_removal() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let first = item(Address::repeat_byte(0x71));
    let second = item(Address::repeat_byte(0x72));
    let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, first.bucket_key);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, &parent, &first, U256::from(5)).unwrap();
        let original = NodContract::new(storage.clone())
            .get_bucket_verified(&scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        api::add_nod(&storage, &scope, &parent, &second, U256::from(5)).unwrap();
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
                .read(&NodContract::bucket_nod_key(first.bucket_key, 0))
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
            U256::from(19)
        );
        assert_eq!(
            nod.ocomp_materialization_tail_sequence.slot(),
            U256::from(20)
        );
    });
}

/// `openings` and the oracle-opening codec both name the snapshot slots; both must be the schema's.
#[test]
fn entry_price_openings_follow_the_schema() {
    use outbe_primitives::storage::types::StorageKey;

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        let frozen = nod.entry_prices_frozen.base_slot();
        let price = nod.entry_price_value.base_slot();
        let day = WorldwideDay::new(20_260_726);
        assert_eq!(
            crate::openings::entry_price_slots(day, &[840]).unwrap(),
            vec![
                B256::from(day.mapping_slot(frozen).to_be_bytes()),
                B256::from(840u16.mapping_slot(day.mapping_slot(price)).to_be_bytes()),
            ]
        );
        let plan = format!("nod_entry_price_slots_v1(frozen={frozen},price={price},");
        assert!(
            outbe_ocomp_protocol::registry::ORACLE_OPENING_CODEC_DESCRIPTOR.contains(&plan),
            "{plan}"
        );
    });
}

#[test]
fn member_count_overflow_and_underflow_roll_back_nod_mutations() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let first = item(Address::repeat_byte(0x73));
    let second = item(Address::repeat_byte(0x74));
    let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, first.bucket_key);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, &parent, &first, U256::from(5)).unwrap();
        let nod = NodContract::new(storage.clone());
        let original = nod
            .get_bucket_verified(&scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        nod.bucket_nod_count
            .write(&first.bucket_key, u32::MAX)
            .unwrap();
        let error = api::add_nod(&storage, &scope, &parent, &second, U256::from(5)).unwrap_err();
        assert!(
            matches!(error, PrecompileError::Fatal(message) if message.contains("member index overflow"))
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
            matches!(error, PrecompileError::BodyReadCorruption(message) if message.contains("member count underflow"))
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

/// Slot assignment is dense in `order` sequence, so inserting a field rather
/// than appending one silently reassigns the meaning of every slot after it -
/// including the two the genesis alloc seeds. New fields must append.
#[test]
fn nod_contract_slot_layout_is_pinned() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        assert_eq!(nod.total_supply.slot(), U256::ZERO);
        assert_eq!(nod.retired_bin_tree_root.base_slot(), U256::from(1));
        assert_eq!(
            nod.retired_unqualified_bin_scan_cursor.base_slot(),
            U256::from(6)
        );
        assert_eq!(nod.bucket_worldwide_day.base_slot(), U256::from(7));
        assert_eq!(nod.ocomp_target_generation.base_slot(), U256::from(8));
        assert_eq!(
            nod.ocomp_materialization_attempt_count.slot(),
            U256::from(23)
        );
        // Call-event columns, appended after the OCOMP block.
        assert_eq!(nod.bucket_nod_count.base_slot(), U256::from(24));
        assert_eq!(nod.bucket_nods.base_slot(), U256::from(25));
        assert_eq!(nod.bucket_nod_index.base_slot(), U256::from(26));
        // `retired_callable_buckets` sits at 27. `StorageVec` exposes no slot
        // accessor, but slots are dense, so pinning 26 and 28 pins it too.
        assert_eq!(
            nod.retired_callable_bucket_index.base_slot(),
            U256::from(28)
        );
        assert_eq!(nod.callable_bucket_call_price.base_slot(), U256::from(29));
        assert_eq!(nod.callable_bucket_currency.base_slot(), U256::from(30));
        assert_eq!(nod.bucket_called_at.base_slot(), U256::from(31));
        assert_eq!(nod.retired_call_scan_cursor.slot(), U256::from(32));
        assert_eq!(
            nod.ocomp_materialization_protocol_bundle_hash.base_slot(),
            U256::from(33)
        );
        // Call terms sealed at issuance, appended after everything above.
        assert_eq!(nod.callable_bucket_call_rate.base_slot(), U256::from(34));
        assert_eq!(nod.callable_bucket_call_window.base_slot(), U256::from(35));
        assert_eq!(
            nod.callable_bucket_call_threshold.base_slot(),
            U256::from(36)
        );
        assert_eq!(
            nod.callable_bucket_call_notice_period.base_slot(),
            U256::from(37)
        );
        assert_eq!(nod.max_call_window.base_slot(), U256::from(38));
        assert_eq!(nod.entry_prices_frozen.base_slot(), U256::from(39));
        assert_eq!(nod.entry_price_currency_count.base_slot(), U256::from(40));
        assert_eq!(nod.entry_price_currency.base_slot(), U256::from(41));
        assert_eq!(nod.entry_price_value.base_slot(), U256::from(42));
        // Issued-at stamp for the call-scan cutoff, appended after everything above.
        assert_eq!(nod.callable_bucket_issued_at.base_slot(), U256::from(43));
        // Frozen-day sweep columns, appended after the issued-at stamp.
        assert_eq!(nod.retired_qualify_sweep_day.slot(), U256::from(44));
        assert_eq!(nod.retired_qualify_pending_day.slot(), U256::from(45));
        assert_eq!(nod.retired_qualify_currency_cursor.slot(), U256::from(46));
        assert_eq!(nod.retired_qualify_scan_cursor.base_slot(), U256::from(47));
        assert_eq!(nod.call_sweep_day.slot(), U256::from(48));
        assert_eq!(nod.call_pending_day.slot(), U256::from(49));
        assert_eq!(nod.call_bin_tree_root.base_slot(), U256::from(50));
        assert_eq!(nod.call_bin_tree_mid.base_slot(), U256::from(51));
        assert_eq!(nod.call_bin_tree_leaf.base_slot(), U256::from(52));
        assert_eq!(nod.call_bin_count.base_slot(), U256::from(53));
        assert_eq!(nod.call_bin_buckets.base_slot(), U256::from(54));
        assert_eq!(nod.call_bucket_bin.base_slot(), U256::from(55));
        // `called_buckets` sits at 56, pinned by its neighbours.
        assert_eq!(nod.called_bucket_index.base_slot(), U256::from(57));
        assert_eq!(nod.forfeit_cursor.slot(), U256::from(58));
        assert_eq!(nod.call_currency_cursor.slot(), U256::from(59));
        assert_eq!(nod.call_bin_cursor.base_slot(), U256::from(60));
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
        issued_at: 1_753_488_000,
        next_nod_ordinal: 129,
        last_progress_height: 4_096,
    };
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let scope = ExecutionScope::new();
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
            &scope,
            &parent,
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
        assert_eq!(actual.nodAmountTotal, generation.nod_amount_total);
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
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let call = crate::precompile::INod::certifiedGenerationCall {
            worldwideDay: worldwide_day.into(),
        }
        .abi_encode();
        let output =
            crate::precompile::dispatch(storage, &scope, &parent, &call, Address::ZERO, U256::ZERO)
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
        assert_eq!(actual.nodAmountTotal, U256::ZERO);
        assert_eq!(actual.lysisAllocationMinor, U256::ZERO);
        assert_eq!(actual.issuedAt, 0);
    });
}

/// Seeds one unqualified bucket denominated in `iso` and returns its identity.
fn seed_bucket(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    owner: Address,
    iso: u16,
) -> WwdEntityId {
    seed_bucket_issued(storage, scope, parent, owner, iso, item(owner).issued_at)
}

fn seed_bucket_issued(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    owner: Address,
    iso: u16,
    issued_at: u64,
) -> WwdEntityId {
    let mut body = item(owner);
    body.reference_currency = iso;
    body.issued_at = issued_at;
    body.bucket_key = NodContract::bucket_key(body.worldwide_day, body.floor_price_minor, iso);
    api::add_nod(storage, scope, parent, &body, U256::from(5)).unwrap();
    WwdEntityId::from_day_and_digest(body.worldwide_day, body.bucket_key)
}

/// Stores the previous completed UTC-day price and finalization watermark.
fn publish_day_vwap(storage: &StorageHandle<'_>, index: u32, rate: U256) {
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let previous_day = previous_date_key(timestamp_to_date_key(NOW));
    oracle
        .utc_day_vwap_value
        .get_nested(&previous_day)
        .write(&index, rate)
        .unwrap();
    oracle
        .utc_day_vwap_last_finalized
        .write(previous_day)
        .unwrap();
}

fn is_qualified(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    bucket_id: WwdEntityId,
) -> bool {
    let bucket = api::get_bucket(storage, scope, parent, bucket_id)
        .unwrap()
        .unwrap();
    api::is_qualified(storage, &bucket).unwrap()
}

/// A currency whose COEN pair was never registered qualifies nothing, without an error.
#[test]
fn an_unregistered_reference_pair_qualifies_nothing() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.reference_currencies.push(978).unwrap();
        publish_day_vwap(&storage, 2, U256::from(14));
        let bucket_id = seed_bucket(&storage, &scope, &parent, Address::repeat_byte(0x66), 978);

        assert!(!is_qualified(&storage, &scope, &parent, bucket_id));
    });
}

/// A registered pair without a daily VWAP qualifies nothing either.
#[test]
fn a_registered_reference_pair_with_no_daily_vwap_qualifies_nothing() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.reference_currencies.push(978).unwrap();
        // Registered, but the finalized day has no VWAP for this pair.
        oracle
            .pair_to_index
            .write(&AddressPair::new_coen_to(978), 1)
            .unwrap();
        publish_day_vwap(&storage, 2, U256::from(14));
        let bucket_id = seed_bucket(&storage, &scope, &parent, Address::repeat_byte(0x66), 978);

        assert!(!is_qualified(&storage, &scope, &parent, bucket_id));
    });
}

/// One unpriced currency does not hold back another.
#[test]
fn a_priced_currency_still_qualifies_when_a_sibling_currency_is_unpriced() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        // 978 comes first and is never priced; 840 follows and is.
        oracle.reference_currencies.push(978).unwrap();
        oracle.reference_currencies.push(840).unwrap();
        oracle
            .pair_to_index
            .write(&AddressPair::new_coen_to(840), 1)
            .unwrap();
        publish_day_vwap(&storage, 1, U256::from(14));

        let unpriced = seed_bucket(&storage, &scope, &parent, Address::repeat_byte(0x66), 978);
        let priced = seed_bucket(&storage, &scope, &parent, Address::repeat_byte(0x77), 840);

        assert!(is_qualified(&storage, &scope, &parent, priced));
        assert!(!is_qualified(&storage, &scope, &parent, unpriced));
    });
}

#[test]
fn qualification_requires_a_finalized_day_above_the_floor_and_stays() {
    for (daily_rate, finalized, live_rate, qualifies) in [
        (0, true, 14, false),   // Missing day; no fallback to a live price.
        (12, true, 14, false),  // A live crossing cannot qualify.
        (13, true, 14, false),  // Equality is not enough.
        (14, false, 14, false), // Wait for Oracle finalization.
        (14, true, 1, true),    // A low live rate cannot prevent qualification.
    ] {
        let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
        let mut provider = HashMapStorageProvider::new(1);
        let scope = ExecutionScope::new();
        StorageHandle::enter(&mut provider, |storage| {
            seed_compressed_entities_genesis(&storage);
            begin_block(storage.clone(), &scope).unwrap();
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            oracle
                .pair_to_index
                .write(&AddressPair::new_coen_to(978), 1)
                .unwrap();
            oracle
                .exchange_rate
                .write(&1, U256::from(live_rate))
                .unwrap();
            oracle.exchange_rate_timestamp.write(&1, NOW).unwrap();
            let current_day = timestamp_to_date_key(NOW);
            let previous_day = previous_date_key(current_day);
            // A day before issuance, an unfinalized day and another currency must
            // not substitute for the bucket's own closes, even above the floor.
            for day in [previous_date_key(previous_day), current_day] {
                oracle
                    .utc_day_vwap_value
                    .get_nested(&day)
                    .write(&1, U256::from(14))
                    .unwrap();
            }
            oracle
                .utc_day_vwap_value
                .get_nested(&previous_day)
                .write(&2, U256::from(14))
                .unwrap();
            oracle
                .utc_day_vwap_value
                .get_nested(&previous_day)
                .write(&1, U256::from(daily_rate))
                .unwrap();
            oracle
                .utc_day_vwap_last_finalized
                .write(if finalized {
                    previous_day
                } else {
                    previous_date_key(previous_day)
                })
                .unwrap();
            let bucket_id = seed_bucket(&storage, &scope, &parent, Address::repeat_byte(0x66), 978);
            assert_eq!(
                is_qualified(&storage, &scope, &parent, bucket_id),
                qualifies
            );

            // The next day closes below the floor: a crossing already made
            // stays, and a day finalized late still counts.
            oracle
                .utc_day_vwap_value
                .get_nested(&current_day)
                .write(&1, U256::from(1))
                .unwrap();
            oracle
                .utc_day_vwap_last_finalized
                .write(current_day)
                .unwrap();
            assert_eq!(
                is_qualified(&storage, &scope, &parent, bucket_id),
                daily_rate > 13
            );
        });
    }
}

/// Q027: the issuance UTC day counts for qualification only when the Nod was
/// issued at midnight. A second later drops that day, so the closed day's
/// VWAP cannot qualify a bucket that did not hold it in full.
#[test]
fn the_issue_day_qualifies_only_for_a_nod_issued_at_midnight() {
    let closed = previous_date_key(timestamp_to_date_key(NOW));
    let midnight = date_key_to_utc_timestamp(closed);
    for (issued_at, expected) in [(midnight, true), (midnight + 1, false)] {
        let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
        let mut provider = HashMapStorageProvider::new(1);
        let scope = ExecutionScope::new();
        StorageHandle::enter(&mut provider, |storage| {
            seed_compressed_entities_genesis(&storage);
            begin_block(storage.clone(), &scope).unwrap();
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            oracle.reference_currencies.push(978).unwrap();
            oracle
                .pair_to_index
                .write(&AddressPair::new_coen_to(978), 1)
                .unwrap();
            publish_day_vwap(&storage, 1, U256::from(14));
            let bucket_id = seed_bucket_issued(
                &storage,
                &scope,
                &parent,
                Address::repeat_byte(0x66),
                978,
                issued_at,
            );
            assert_eq!(
                is_qualified(&storage, &scope, &parent, bucket_id),
                expected,
                "issued at {issued_at}"
            );
        });
    }
}

/// Q027: a delayed materialization must not qualify on a pre-issuance close,
/// even when that day's VWAP stands above the floor. Issued at `NOW` (not
/// midnight), the last closed day is two calendar days before
/// `first_full_day(issued_at)`.
#[test]
fn a_delayed_issuance_does_not_qualify_on_pre_issuance_days() {
    let issued_at = NOW;
    assert!(
        previous_date_key(timestamp_to_date_key(NOW)) < first_full_day(issued_at),
        "the fixture must evaluate a day the bucket has not held in full"
    );
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.reference_currencies.push(978).unwrap();
        oracle
            .pair_to_index
            .write(&AddressPair::new_coen_to(978), 1)
            .unwrap();
        publish_day_vwap(&storage, 1, U256::from(14));
        let bucket_id = seed_bucket_issued(
            &storage,
            &scope,
            &parent,
            Address::repeat_byte(0x66),
            978,
            issued_at,
        );
        assert!(!is_qualified(&storage, &scope, &parent, bucket_id));
    });
}

/// A close that predates `first_full_day` does not qualify the bucket; the first
/// close of a day it held in full does.
#[test]
fn a_bucket_qualifies_on_its_first_full_day_after_skipping_earlier_closes() {
    let issued_at = NOW;
    let full = first_full_day(issued_at);
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.reference_currencies.push(978).unwrap();
        oracle
            .pair_to_index
            .write(&AddressPair::new_coen_to(978), 1)
            .unwrap();
        publish_day_vwap(&storage, 1, U256::from(14));
        let bucket_id = seed_bucket_issued(
            &storage,
            &scope,
            &parent,
            Address::repeat_byte(0x66),
            978,
            issued_at,
        );
        assert!(!is_qualified(&storage, &scope, &parent, bucket_id));

        publish_vwap_on(&storage, 1, full, U256::from(14));
        assert!(is_qualified(&storage, &scope, &parent, bucket_id));
    });
}

/// A bucket in a currency without a priced pair is never qualified by another currency's rate.
#[test]
fn a_bucket_in_an_unlisted_currency_stays_unqualified_and_intact() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.reference_currencies.push(840).unwrap();
        oracle
            .pair_to_index
            .write(&AddressPair::new_coen_to(840), 1)
            .unwrap();
        publish_day_vwap(&storage, 1, U256::from(14));

        let unlisted = seed_bucket(&storage, &scope, &parent, Address::repeat_byte(0x66), 978);
        assert!(!is_qualified(&storage, &scope, &parent, unlisted));
        assert!(
            !NodContract::new(storage.clone())
                .call_bin_tree_root
                .read(&978)
                .unwrap()
                .is_zero(),
            "the unlisted currency's bin entry must survive untouched"
        );
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

        nod.clear_ocomp_certified_generation(worldwide_day).unwrap();
        assert!(
            nod.ocomp_certified_generation(worldwide_day)
                .unwrap()
                .is_none(),
            "clearing has to wipe the bundle too, or the day reads as residual state"
        );
    });
}

fn publish_vwap_on(storage: &StorageHandle<'_>, index: u32, day: u32, rate: U256) {
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    oracle
        .utc_day_vwap_value
        .get_nested(&day)
        .write(&index, rate)
        .unwrap();
    oracle.utc_day_vwap_last_finalized.write(day).unwrap();
}
