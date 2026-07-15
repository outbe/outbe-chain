use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{CommitmentState, EntityId36};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_primitives::addresses::NOD_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

use crate::{api, NodItemState, NodRepositoryReader, NodRepositoryWriter};

fn item(owner: Address) -> NodItemState {
    let worldwide_day = WorldwideDay::new(20_260_715);
    NodItemState {
        nod_id: crate::NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
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
fn issuance_commits_before_verified_reads_and_removal_clears_absence() {
    let storage = Arc::new(MemoryStorage::new());
    let read: StorageReaderHandle = storage.clone();
    let write: StorageWriterHandle = storage;
    let reader = NodRepositoryReader::new(read.clone());
    let writer = NodRepositoryWriter::new(read, write);
    let body = item(Address::repeat_byte(0x22));
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |evm| {
        api::add_nod(&evm, &reader, &body, U256::from(5)).unwrap();
        assert!(CommitmentState::new(evm.clone())
            .nod_item(body.nod_id)
            .unwrap()
            .is_some());
        assert!(api::get_item(&evm, &reader, body.nod_id).is_err());
    });

    writer.put_nod(&body).unwrap();
    let bucket = crate::NodBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        floor_price_minor: body.floor_price_minor,
        is_qualified: false,
        total_nods: 1,
        entry_price_minor: U256::from(5),
    };
    writer.put_bucket(&bucket).unwrap();

    StorageHandle::enter(&mut provider, |evm| {
        assert_eq!(
            api::get_item(&evm, &reader, body.nod_id)
                .unwrap()
                .unwrap()
                .owner,
            body.owner
        );
        api::remove_nod(&evm, &reader, &body).unwrap();
        assert!(api::get_item(&evm, &reader, body.nod_id).unwrap().is_none());
    });
}

#[test]
fn nod_identity_text_roundtrip_preserves_all_36_bytes() {
    let body = item(Address::repeat_byte(0x33));
    let encoded = crate::NodContract::format_nod_id(body.nod_id);
    assert_eq!(
        crate::NodContract::parse_nod_id(&encoded).unwrap(),
        body.nod_id
    );
    assert!(crate::NodContract::parse_nod_id(&encoded[..70]).is_err());
}

#[test]
fn abi_rejects_non_36_byte_identity_before_execution_read() {
    let storage = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(storage);
    let mut provider = HashMapStorageProvider::new(1);
    let short_call = crate::precompile::INod::ownerOfCall {
        nodId: Bytes::from(vec![0x11; 35]),
    }
    .abi_encode();

    StorageHandle::enter(&mut provider, |evm| {
        let error = crate::precompile::dispatch_with_reader(
            evm.clone(),
            &short_call,
            Address::ZERO,
            U256::ZERO,
            &reader,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Revert(ref reason)
                if reason == "invalid bytes length: expected 36"
        ));
    });
}

#[test]
fn reverted_issuance_rolls_back_commitments_compact_state_and_events() {
    let storage = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(storage);
    let body = item(Address::repeat_byte(0x66));
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |evm| {
        let outcome: Result<()> = evm.with_checkpoint(|| {
            api::add_nod(&evm, &reader, &body, U256::from(5))?;
            assert!(CommitmentState::new(evm.clone())
                .nod_item(body.nod_id)?
                .is_some());
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(outcome.is_err());
        assert_eq!(
            crate::NodContract::new(evm.clone()).total_supply().unwrap(),
            0
        );
        assert!(CommitmentState::new(evm.clone())
            .nod_item(body.nod_id)
            .unwrap()
            .is_none());
        assert!(CommitmentState::new(evm.clone())
            .nod_bucket(EntityId36::new(body.worldwide_day, body.bucket_key.0))
            .unwrap()
            .is_none());
    });
    assert!(provider.get_events(NOD_ADDRESS).is_empty());
}

#[test]
fn out_of_gas_issuance_rolls_back_item_and_bucket_commitments() {
    let storage = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(storage);
    let body = item(Address::repeat_byte(0x67));
    let bucket_id = EntityId36::new(body.worldwide_day, body.bucket_key.0);
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |evm| {
        let outcome: Result<()> = evm.with_checkpoint(|| {
            api::add_nod(&evm, &reader, &body, U256::from(5))?;
            let commitments = CommitmentState::new(evm.clone());
            assert!(commitments.nod_item(body.nod_id)?.is_some());
            assert!(commitments.nod_bucket(bucket_id)?.is_some());
            Err(PrecompileError::OutOfGas)
        });
        assert!(matches!(outcome, Err(PrecompileError::OutOfGas)));
        let commitments = CommitmentState::new(evm.clone());
        assert!(commitments.nod_item(body.nod_id).unwrap().is_none());
        assert!(commitments.nod_bucket(bucket_id).unwrap().is_none());
        assert_eq!(
            crate::NodContract::new(evm.clone()).total_supply().unwrap(),
            0
        );
    });
    assert!(provider.get_events(NOD_ADDRESS).is_empty());
}

#[test]
fn reverted_bucket_update_restores_prior_commitment_and_worklist() {
    let storage = Arc::new(MemoryStorage::new());
    let read: StorageReaderHandle = storage.clone();
    let write: StorageWriterHandle = storage;
    let reader = NodRepositoryReader::new(read.clone());
    let writer = NodRepositoryWriter::new(read, write);
    let body = item(Address::repeat_byte(0x68));
    let bucket_id = EntityId36::new(body.worldwide_day, body.bucket_key.0);
    let bucket = crate::NodBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        floor_price_minor: body.floor_price_minor,
        is_qualified: false,
        total_nods: 1,
        entry_price_minor: U256::from(5),
    };
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |evm| {
        api::add_nod(&evm, &reader, &body, bucket.entry_price_minor).unwrap();
    });
    writer.put_nod(&body).unwrap();
    writer.put_bucket(&bucket).unwrap();
    provider.clear_events(NOD_ADDRESS);

    for out_of_gas in [false, true] {
        StorageHandle::enter(&mut provider, |evm| {
            let before = CommitmentState::new(evm.clone())
                .nod_bucket(bucket_id)
                .unwrap()
                .unwrap();
            let outcome: Result<()> = evm.with_checkpoint(|| {
                let context = BlockRuntimeContext::new(
                    BlockContext::empty_for_tests(2, 1_752_534_001, 1),
                    evm.clone(),
                );
                crate::hooks::qualify_buckets_with_rate_and_reader(
                    &context,
                    &reader,
                    body.floor_price_minor + U256::from(1),
                )?;
                let during = CommitmentState::new(evm.clone())
                    .nod_bucket(bucket_id)?
                    .unwrap();
                assert_ne!(during, before);
                if out_of_gas {
                    Err(PrecompileError::OutOfGas)
                } else {
                    Err(PrecompileError::Revert("transaction reverted".into()))
                }
            });
            assert!(outcome.is_err());
            assert_eq!(
                CommitmentState::new(evm.clone())
                    .nod_bucket(bucket_id)
                    .unwrap(),
                Some(before)
            );
            assert!(
                !api::get_bucket(&evm, &reader, bucket_id)
                    .unwrap()
                    .unwrap()
                    .is_qualified
            );
        });
        assert!(provider.get_events(NOD_ADDRESS).is_empty());
    }
}

#[test]
fn failed_removal_transaction_restores_item_bucket_compact_state_and_events() {
    let storage = Arc::new(MemoryStorage::new());
    let read: StorageReaderHandle = storage.clone();
    let write: StorageWriterHandle = storage;
    let reader = NodRepositoryReader::new(read.clone());
    let writer = NodRepositoryWriter::new(read, write);
    let body = item(Address::repeat_byte(0x69));
    let bucket_id = EntityId36::new(body.worldwide_day, body.bucket_key.0);
    let bucket = crate::NodBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        floor_price_minor: body.floor_price_minor,
        is_qualified: false,
        total_nods: 1,
        entry_price_minor: U256::from(5),
    };
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |evm| {
        api::add_nod(&evm, &reader, &body, bucket.entry_price_minor).unwrap();
    });
    writer.put_nod(&body).unwrap();
    writer.put_bucket(&bucket).unwrap();
    provider.clear_events(NOD_ADDRESS);

    for out_of_gas in [false, true] {
        StorageHandle::enter(&mut provider, |evm| {
            let commitments = CommitmentState::new(evm.clone());
            let item_before = commitments.nod_item(body.nod_id).unwrap().unwrap();
            let bucket_before = commitments.nod_bucket(bucket_id).unwrap().unwrap();
            let failed: Result<()> = evm.with_checkpoint(|| {
                api::remove_nod(&evm, &reader, &body)?;
                let during = CommitmentState::new(evm.clone());
                assert!(during.nod_item(body.nod_id)?.is_none());
                assert!(during.nod_bucket(bucket_id)?.is_none());
                if out_of_gas {
                    Err(PrecompileError::OutOfGas)
                } else {
                    Err(PrecompileError::Revert("transaction reverted".into()))
                }
            });
            assert!(failed.is_err());
            let commitments = CommitmentState::new(evm.clone());
            assert_eq!(
                commitments.nod_item(body.nod_id).unwrap(),
                Some(item_before)
            );
            assert_eq!(
                commitments.nod_bucket(bucket_id).unwrap(),
                Some(bucket_before)
            );
            assert_eq!(
                crate::NodContract::new(evm.clone()).total_supply().unwrap(),
                1
            );
            assert_eq!(
                api::get_item(&evm, &reader, body.nod_id)
                    .unwrap()
                    .unwrap()
                    .owner,
                body.owner
            );
        });
        assert!(provider.get_events(NOD_ADDRESS).is_empty());
    }
}
