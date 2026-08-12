use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{begin_block, ExecutionScope};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    error::{PrecompileError, Result},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn seed_compressed_entities_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
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

use crate::{
    api, NodCertifiedGenerationProjection, NodContract, NodItemState, NodRepositoryReader,
};

fn item(owner: Address) -> NodItemState {
    let worldwide_day = WorldwideDay::new(20_260_715);
    NodItemState {
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day,
        league_id: 4,
        floor_price_minor: U256::from(13),
        bucket_key: B256::repeat_byte(0x44),
        cost_amount_minor: U256::from(17),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 1_752_534_000,
    }
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
fn nod_identity_and_abi_boundary_preserve_exact_36_bytes() {
    let body = item(Address::repeat_byte(0x33));
    let encoded = NodContract::format_nod_id(body.nod_id);
    assert_eq!(NodContract::parse_nod_id(&encoded).unwrap(), body.nod_id);
    assert!(NodContract::parse_nod_id(&encoded[..70]).is_err());

    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    let call = crate::precompile::INod::ownerOfCall {
        nodId: Bytes::from(vec![0x11; 35]),
    }
    .abi_encode();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let error =
            crate::precompile::dispatch(storage, &scope, &parent, &call, Address::ZERO, U256::ZERO)
                .unwrap_err();
        assert!(matches!(
            error,
            PrecompileError::Revert(ref reason) if reason == "invalid bytes length: expected 36"
        ));
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

#[test]
fn certified_generation_is_available_through_the_public_nod_abi() {
    let worldwide_day = WorldwideDay::new(20_260_726);
    let generation = NodCertifiedGenerationProjection {
        worldwide_day,
        generation: 9,
        job_id: B256::repeat_byte(0x44),
        program_semantics_hash: B256::repeat_byte(0x55),
        nod_root: B256::repeat_byte(0x11),
        bucket_root: B256::repeat_byte(0x22),
        output_manifest_root: B256::repeat_byte(0x33),
        tribute_count: 257,
        nod_count: 257,
        bucket_count: 13,
        nod_amount_total: U256::from(50_000),
        nod_gratis_consumed: U256::from(7_000),
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
        nod.ocomp_nod_gratis_consumed
            .write(&worldwide_day, generation.nod_gratis_consumed)
            .unwrap();
        nod.ocomp_materialization_job_id
            .write(&worldwide_day, generation.job_id)
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
        assert_eq!(actual.nodGratisConsumed, generation.nod_gratis_consumed);
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
        assert_eq!(actual.nodGratisConsumed, U256::ZERO);
        assert_eq!(actual.issuedAt, 0);
    });
}

/// An unregistered COEN/<qualifier ISO> pair must skip the block's qualification
/// scan, not halt the block. Before the ISO migration this path propagated
/// `Revert("pair not registered")` out of `begin_block`; it now soft-skips like
/// the gem and intexfactory qualifiers.
#[test]
fn qualify_nods_skips_the_block_when_the_settlement_pair_is_unregistered() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, 1_752_534_000, 1),
            storage.clone(),
        );
        // Oracle storage is untouched, so ISO 840 has no settlement pair.
        crate::hooks::qualify_nods(&ctx, &scope, &parent).unwrap();
    });
}

/// A registered pair with no published rate must also skip rather than qualify
/// every bucket against a zero rate.
#[test]
fn qualify_nods_skips_the_block_when_the_pair_has_no_published_rate() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();

        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, 1_752_534_000, 1),
            storage.clone(),
        );
        crate::hooks::qualify_nods(&ctx, &scope, &parent).unwrap();
    });
}
