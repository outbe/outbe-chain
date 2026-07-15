use std::sync::Arc;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{CommitmentState, EntityId36};
use outbe_nod::{
    api as nod_api, precompile::INod, NodBucketState, NodContract, NodIssueParams, NodItemState,
    NodRepositoryReader, NodRepositoryWriter,
};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_primitives::addresses::{NOD_ADDRESS, NOD_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

use crate::api as factory_api;
use crate::precompile::{dispatch_with_reader, INodFactory};
use crate::runtime;

const T_NOW: u64 = 1_700_000_000;
const PAY_ASSET: Address = address!("000000000000000000000000000000000000A11C");

fn sample_params() -> NodIssueParams {
    NodIssueParams {
        owner: address!("1111111111111111111111111111111111111111"),
        gratis_load_minor: U256::from(1_000_000_000_000_000_000_u128),
        worldwide_day: WorldwideDay::new(20_241_220),
        league_id: 1,
        floor_price_minor: U256::from(540_000_000_000_000_000_u128),
        entry_price_minor: U256::from(500_000_000_000_000_000_u128),
        cost_amount_minor: U256::ZERO,
        issuance_currency: 840,
        reference_currency: 840,
    }
}

fn find_valid_nonce(nod_id: EntityId36) -> U256 {
    for value in 0_u64..100_000 {
        let nonce = U256::from(value);
        if runtime::validate_pow(nod_id, nonce).is_ok() {
            return nonce;
        }
    }
    panic!("could not find a valid nonce in 100k attempts");
}

struct IssuedNod {
    item: NodItemState,
    bucket: NodBucketState,
}

struct World {
    provider: HashMapStorageProvider,
    reader: NodRepositoryReader,
    writer: NodRepositoryWriter,
}

impl World {
    fn new() -> Self {
        let storage = Arc::new(MemoryStorage::new());
        let reader_handle: StorageReaderHandle = storage.clone();
        let writer_handle: StorageWriterHandle = storage;
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(T_NOW));
        Self {
            provider,
            reader: NodRepositoryReader::new(reader_handle.clone()),
            writer: NodRepositoryWriter::new(reader_handle, writer_handle),
        }
    }

    fn enter<R>(&mut self, call: impl FnOnce(StorageHandle<'_>, &NodRepositoryReader) -> R) -> R {
        let reader = self.reader.clone();
        StorageHandle::enter(&mut self.provider, |storage| call(storage, &reader))
    }

    fn issue_unprojected(&mut self, params: &NodIssueParams) -> IssuedNod {
        let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day).unwrap();
        let bucket_key = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
        let bucket_id = EntityId36::new(params.worldwide_day, bucket_key.0);
        let previous_bucket = self.reader.get_bucket(bucket_id).unwrap();
        let issued = self.enter(|storage, reader| {
            factory_api::issue_nod_with_reader(&storage, reader, params).unwrap()
        });
        assert_eq!(issued, nod_id);

        let item = NodItemState {
            nod_id,
            owner: params.owner,
            gratis_load_minor: params.gratis_load_minor,
            worldwide_day: params.worldwide_day,
            league_id: params.league_id,
            floor_price_minor: params.floor_price_minor,
            bucket_key,
            cost_amount_minor: params.cost_amount_minor,
            issuance_currency: params.issuance_currency,
            reference_currency: params.reference_currency,
            issued_at: T_NOW,
        };
        let bucket = match previous_bucket {
            Some(mut bucket) => {
                bucket.total_nods += 1;
                bucket
            }
            None => NodBucketState {
                bucket_key,
                worldwide_day: params.worldwide_day,
                floor_price_minor: params.floor_price_minor,
                is_qualified: false,
                total_nods: 1,
                entry_price_minor: params.entry_price_minor,
            },
        };
        IssuedNod { item, bucket }
    }

    fn project_issue(&self, issued: &IssuedNod) {
        self.writer.put_nod(&issued.item).unwrap();
        self.writer.put_bucket(&issued.bucket).unwrap();
    }

    fn issue(&mut self, params: &NodIssueParams) -> EntityId36 {
        let issued = self.issue_unprojected(params);
        let nod_id = issued.item.nod_id;
        self.project_issue(&issued);
        nod_id
    }

    fn qualify(&mut self, params: &NodIssueParams) {
        let bucket_key = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
        let bucket_id = EntityId36::new(params.worldwide_day, bucket_key.0);
        let mut bucket = self.reader.get_bucket(bucket_id).unwrap().unwrap();
        self.enter(|storage, reader| {
            NodContract::new(storage)
                .qualify_bucket_with_reader(reader, bucket_key)
                .unwrap();
        });
        bucket.is_qualified = true;
        self.writer.put_bucket(&bucket).unwrap();
    }

    fn mine(
        &mut self,
        caller: Address,
        nod_id: EntityId36,
        nonce: U256,
        asset: Address,
    ) -> Result<U256> {
        let item = self.reader.get(nod_id).unwrap().unwrap();
        let bucket_id = EntityId36::new(item.worldwide_day, item.bucket_key.0);
        let mut bucket = self.reader.get_bucket(bucket_id).unwrap().unwrap();
        let result = self.enter(|storage, reader| {
            factory_api::mine_gratis_with_reader(&storage, reader, caller, nod_id, nonce, asset)
        });
        if result.is_ok() {
            self.writer.delete_nod(nod_id).unwrap();
            bucket.total_nods -= 1;
            if bucket.total_nods == 0 {
                self.writer.delete_bucket(bucket_id).unwrap();
            } else {
                self.writer.put_bucket(&bucket).unwrap();
            }
        }
        result
    }

    fn item_commitment(
        &mut self,
        nod_id: EntityId36,
    ) -> Option<outbe_compressed_entities::Commitment> {
        self.enter(|storage, _| CommitmentState::new(storage).nod_item(nod_id).unwrap())
    }

    fn clear_product_events(&mut self) {
        self.provider.clear_events(NOD_ADDRESS);
        self.provider.clear_events(NOD_FACTORY_ADDRESS);
    }
}

fn assert_ordered_events(provider: &HashMapStorageProvider, expected: &[(Address, B256)]) {
    let events: Vec<_> = provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_ADDRESS || event.address == NOD_FACTORY_ADDRESS)
        .collect();
    assert_eq!(events.len(), expected.len());
    for (event, (address, signature)) in events.iter().zip(expected) {
        assert_eq!(event.address, *address);
        assert_eq!(event.data.topics()[0], *signature);
    }
}

#[test]
fn issuance_commits_then_repository_projection_enables_verified_reads() {
    let mut world = World::new();
    let params = sample_params();
    let issued = world.issue_unprojected(&params);

    assert!(world.item_commitment(issued.item.nod_id).is_some());
    let error = match world
        .enter(|storage, reader| nod_api::get_item(&storage, reader, issued.item.nod_id))
    {
        Ok(_) => panic!("committed body must not read successfully before projection"),
        Err(error) => error,
    };
    assert!(
        matches!(error, PrecompileError::BodyReadCorruption(message) if message.contains("CommittedBodyMissing"))
    );

    world.project_issue(&issued);
    let stored = world
        .enter(|storage, reader| nod_api::get_item(&storage, reader, issued.item.nod_id))
        .unwrap()
        .unwrap();
    assert_eq!(stored.owner, params.owner);
    assert_eq!(stored.worldwide_day, params.worldwide_day);
    assert_eq!(stored.league_id, params.league_id);
    assert_eq!(stored.floor_price_minor, params.floor_price_minor);
    assert_eq!(stored.gratis_load_minor, params.gratis_load_minor);
    assert_eq!(
        world.enter(|storage, _| NodContract::new(storage).total_supply().unwrap()),
        1
    );
    assert_ordered_events(
        &world.provider,
        &[
            (NOD_ADDRESS, INod::NodBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyStored::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodIssued::SIGNATURE_HASH),
        ],
    );
}

#[test]
fn duplicate_and_invalid_owner_issuance_fail_without_events() {
    let mut world = World::new();
    let params = sample_params();
    world.issue(&params);
    world.clear_product_events();

    let duplicate = world
        .enter(|storage, reader| factory_api::issue_nod_with_reader(&storage, reader, &params));
    assert!(
        matches!(duplicate, Err(PrecompileError::Revert(message)) if message == "nod already exists")
    );
    assert!(world.provider.get_events(NOD_ADDRESS).is_empty());
    assert!(world.provider.get_events(NOD_FACTORY_ADDRESS).is_empty());

    let mut invalid = params;
    invalid.owner = Address::ZERO;
    let error = world
        .enter(|storage, reader| factory_api::issue_nod_with_reader(&storage, reader, &invalid))
        .unwrap_err();
    assert!(matches!(error, PrecompileError::Revert(message) if message == "invalid owner"));
}

#[test]
fn bucket_projection_preserves_explicit_entry_price() {
    let mut world = World::new();
    let mut params = sample_params();
    params.entry_price_minor = U256::from(101);
    params.floor_price_minor = U256::from(109);
    world.issue(&params);

    let bucket_key = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
    let bucket_id = EntityId36::new(params.worldwide_day, bucket_key.0);
    let bucket = world.reader.get_bucket(bucket_id).unwrap().unwrap();
    assert_eq!(bucket.total_nods, 1);
    assert_eq!(bucket.floor_price_minor, params.floor_price_minor);
    assert!(!bucket.is_qualified);
    assert_eq!(bucket.entry_price_minor, U256::from(101));
}

#[test]
fn mining_requires_owner_pow_and_qualification_without_partial_mutation() {
    let mut world = World::new();
    let params = sample_params();
    let nod_id = world.issue(&params);
    let nonce = find_valid_nonce(nod_id);
    world.clear_product_events();

    let wrong_owner = address!("9999999999999999999999999999999999999999");
    assert!(world
        .mine(wrong_owner, nod_id, nonce, Address::ZERO)
        .is_err());
    assert!(world.reader.get(nod_id).unwrap().is_some());

    let bad_nonce = (0_u64..100_000)
        .map(U256::from)
        .find(|nonce| runtime::validate_pow(nod_id, *nonce).is_err())
        .unwrap();
    assert!(world
        .mine(params.owner, nod_id, bad_nonce, Address::ZERO)
        .is_err());
    assert!(world.reader.get(nod_id).unwrap().is_some());

    let error = world
        .mine(params.owner, nod_id, nonce, Address::ZERO)
        .unwrap_err();
    assert!(matches!(error, PrecompileError::Revert(message) if message == "nod is not qualified"));
    assert!(world.item_commitment(nod_id).is_some());
    assert!(world.provider.get_events(NOD_ADDRESS).is_empty());
    assert!(world.provider.get_events(NOD_FACTORY_ADDRESS).is_empty());
}

#[test]
fn mining_clears_commitment_projects_deletion_and_mints_gratis() {
    let mut world = World::new();
    let params = sample_params();
    let nod_id = world.issue(&params);
    world.qualify(&params);
    world.clear_product_events();

    let minted = world
        .mine(
            params.owner,
            nod_id,
            find_valid_nonce(nod_id),
            Address::ZERO,
        )
        .unwrap();
    assert_eq!(minted, params.gratis_load_minor);
    assert!(world.item_commitment(nod_id).is_none());
    assert!(world.reader.get(nod_id).unwrap().is_none());
    assert_eq!(
        world.enter(|storage, _| NodContract::new(storage.clone()).total_supply().unwrap()),
        0
    );
    assert_eq!(
        world.enter(|storage, _| outbe_gratis::Gratis::new(storage)
            .balance_of(params.owner)
            .unwrap()),
        minted
    );
    assert_ordered_events(
        &world.provider,
        &[
            (NOD_ADDRESS, INod::NodBodyDeleted::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyDeleted::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodBurned::SIGNATURE_HASH),
        ],
    );
}

#[test]
fn owner_repository_index_remains_dense_after_mining_one_of_two_nods() {
    let mut world = World::new();
    let first = sample_params();
    let mut second = sample_params();
    second.worldwide_day = WorldwideDay::new(first.worldwide_day.value() + 1);
    let first_id = world.issue(&first);
    let second_id = world.issue(&second);
    world.qualify(&first);
    world
        .mine(
            first.owner,
            first_id,
            find_valid_nonce(first_id),
            Address::ZERO,
        )
        .unwrap();

    let remaining = world
        .enter(|storage, reader| nod_api::list_by_owner(&storage, reader, first.owner))
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].nod_id, second_id);
}

#[test]
fn failed_gratis_mint_rolls_back_commitment_and_events() {
    let mut world = World::new();
    let params = sample_params();
    let nod_id = world.issue(&params);
    world.qualify(&params);
    world.enter(|storage, _| {
        outbe_gratis::Gratis::new(storage)
            .total_supply
            .write(U256::MAX)
            .unwrap();
    });
    world.clear_product_events();

    let nonce = find_valid_nonce(nod_id);
    let result = world.enter(|storage, reader| {
        storage.with_checkpoint(|| {
            factory_api::mine_gratis_with_reader(
                &storage,
                reader,
                params.owner,
                nod_id,
                nonce,
                Address::ZERO,
            )
        })
    });
    assert!(result.unwrap_err().to_string().contains("overflow"));
    assert!(world.item_commitment(nod_id).is_some());
    assert!(world.reader.get(nod_id).unwrap().is_some());
    assert!(world.provider.get_events(NOD_ADDRESS).is_empty());
    assert!(world.provider.get_events(NOD_FACTORY_ADDRESS).is_empty());
}

#[test]
fn nonzero_cost_requires_asset_and_executes_payment_subcalls() {
    let mut world = World::new();
    world.provider.enable_sub_call_stub();
    world
        .provider
        .stub_sub_call_at(VAULT_PROVIDER_ADDRESS, Bytes::from(vec![0_u8; 32]));
    let mut params = sample_params();
    params.cost_amount_minor = U256::from(500_000_000_000_000_000_u128);
    let nod_id = world.issue(&params);
    world.qualify(&params);
    let nonce = find_valid_nonce(nod_id);

    let error = world
        .mine(params.owner, nod_id, nonce, Address::ZERO)
        .unwrap_err();
    assert!(matches!(error, PrecompileError::Revert(message) if message == "invalid asset"));
    assert!(world.reader.get(nod_id).unwrap().is_some());

    let minted = world.mine(params.owner, nod_id, nonce, PAY_ASSET).unwrap();
    assert_eq!(minted, params.gratis_load_minor);
    assert!(world.reader.get(nod_id).unwrap().is_none());
}

#[test]
fn zero_cost_skips_payment_subcalls() {
    let mut world = World::new();
    let params = sample_params();
    let nod_id = world.issue(&params);
    world.qualify(&params);
    let minted = world
        .mine(
            params.owner,
            nod_id,
            find_valid_nonce(nod_id),
            Address::ZERO,
        )
        .unwrap();
    assert_eq!(minted, params.gratis_load_minor);
}

#[test]
fn precompile_uses_bytes36_and_reader_backed_mining() {
    let mut world = World::new();
    let params = sample_params();
    let nod_id = world.issue(&params);
    world.qualify(&params);
    let call = INodFactory::mineGratisCall {
        nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
        nonce: find_valid_nonce(nod_id),
        asset: Address::ZERO,
    };
    let calldata = call.abi_encode();
    let output = world
        .enter(|storage, reader| {
            dispatch_with_reader(storage, &calldata, params.owner, U256::ZERO, reader)
        })
        .unwrap();
    let minted = INodFactory::mineGratisCall::abi_decode_returns(&output).unwrap();
    assert_eq!(minted, params.gratis_load_minor);
    assert!(world.item_commitment(nod_id).is_none());

    let malformed = INodFactory::mineGratisCall {
        nodId: Bytes::from(vec![0_u8; 35]),
        nonce: U256::ZERO,
        asset: Address::ZERO,
    }
    .abi_encode();
    assert!(world
        .enter(|storage, reader| {
            dispatch_with_reader(storage, &malformed, params.owner, U256::ZERO, reader)
        })
        .is_err());
}

#[test]
fn precompile_rejects_msg_value_before_mining() {
    let mut world = World::new();
    let params = sample_params();
    let nod_id = world.issue(&params);
    let calldata = INodFactory::mineGratisCall {
        nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
        nonce: U256::ZERO,
        asset: Address::ZERO,
    }
    .abi_encode();
    let error = world
        .enter(|storage, reader| {
            dispatch_with_reader(storage, &calldata, params.owner, U256::from(1), reader)
        })
        .unwrap_err();
    assert!(matches!(error, PrecompileError::Revert(_)));
    assert!(world.item_commitment(nod_id).is_some());
}

#[test]
fn bin_tree_root_survives_qualification_and_mining() {
    let mut world = World::new();
    let params = sample_params();
    let nod_id = world.issue(&params);
    let bin_id = NodContract::price_to_bin(params.floor_price_minor).unwrap();
    assert!(world
        .enter(|storage, _| { tree_math::contains(&NodContract::new(storage), bin_id).unwrap() }));
    world.qualify(&params);
    world
        .mine(
            params.owner,
            nod_id,
            find_valid_nonce(nod_id),
            Address::ZERO,
        )
        .unwrap();
}

#[test]
fn pow_hash_uses_the_complete_entity_id_and_u64_nonce() {
    let params = sample_params();
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day).unwrap();
    let nonce = U256::from(42);
    let actual = runtime::compute_pow_hash(nod_id, nonce).unwrap();
    let mut input = NodContract::format_nod_id(nod_id).into_bytes();
    input.extend_from_slice(&42_u64.to_be_bytes());
    let expected = ring::digest::digest(&ring::digest::SHA256, &input);
    assert_eq!(actual.as_slice(), expected.as_ref());
}
