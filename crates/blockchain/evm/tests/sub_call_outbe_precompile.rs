//! sub-call → outbe precompile visibility test.
//!
//! Verifies that after outbe-precompile propagation, a sub-call driven by
//! `sub_call::run` can reach an outbe stateful precompile and returns its
//! output. The target is the stateless Poseidon-BN254 hash precompile at
//! `ZKPROOF_POSEIDON_ADDRESS` (`0xEE07`), chosen because it needs no state or
//! contract setup: raw bytes in, 32-byte hash out.
//!
//! This proves the child frame uses `OutbeSubCallPrecompiles` (which dispatches
//! outbe addresses) rather than plain `EthPrecompiles`. With the old wiring the
//! call to `0xEE07` would fall through to an empty-account call and return
//! `Success` with empty returndata; here we assert the returndata equals the
//! Poseidon hash of the input.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    begin_block, body_commitment, encode_nod_item_v1, AuthenticatedParentTree, CeWorkConfig,
    Commitment, EntityRef, ExecutionScope, FinalLeafMutation, ProvisionalTreeBatch,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_evm::sub_call;
use outbe_nod::{precompile::INod, NodBucketState, NodItemState, NodRepositoryWriter};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_primitives::addresses::{NOD_ADDRESS, ZKPROOF_POSEIDON_ADDRESS};
use outbe_primitives::{
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallStatus},
};
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    Context,
};

const CALLER: Address = Address::new([0xC0; 20]);

#[derive(Debug)]
struct StaticAuthenticatedParent {
    entity: EntityRef,
    commitment: Commitment,
}

impl AuthenticatedParentTree for StaticAuthenticatedParent {
    fn parent_block_hash(&self) -> B256 {
        B256::ZERO
    }

    fn parent_root(&self) -> B256 {
        B256::ZERO
    }

    fn read_leaf_verified(
        &self,
        entity: EntityRef,
        expected_parent_root: B256,
    ) -> outbe_primitives::error::Result<Option<Commitment>> {
        assert_eq!(expected_parent_root, B256::ZERO);
        Ok((entity == self.entity).then_some(self.commitment))
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        _mutations: &[FinalLeafMutation],
    ) -> outbe_primitives::error::Result<ProvisionalTreeBatch> {
        ProvisionalTreeBatch::new_unsharded(
            block_number,
            B256::ZERO,
            B256::ZERO,
            B256::ZERO,
            Default::default(),
            Default::default(),
        )
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))
    }
}

#[test]
fn subcall_reaches_outbe_poseidon_precompile() {
    // No account is inserted for the precompile address: outbe precompiles are
    // dispatched by the provider, not backed by real accounts/bytecode.
    let mut ctx = Context::mainnet().with_db(CacheDB::new(EmptyDB::default()));

    // One 32-byte BN254 field element as Poseidon input.
    let mut input = [0u8; 32];
    input[31] = 0x2a;
    let calldata = Bytes::from(input.to_vec());

    let result = sub_call::run(
        &mut ctx,
        CALLER,
        /* outer_is_static = */ false,
        SpecId::PRAGUE,
        None,
        Arc::new(ExecutionScope::new()),
        SubCallInput {
            target: ZKPROOF_POSEIDON_ADDRESS,
            value: U256::ZERO,
            calldata,
            gas_limit: 1_000_000,
            is_static: true,
        },
    )
    .expect("sub_call should not return a fatal error");

    assert!(
        matches!(result.status, SubCallStatus::Success),
        "expected Success, got {:?}",
        result.status,
    );

    // The child frame must have dispatched the outbe Poseidon precompile, so
    // the returndata is the 32-byte hash of the input (not an empty payload).
    let expected = outbe_zkproof::poseidon::poseidon_hash(&input)
        .expect("poseidon hash over one field element");
    assert_eq!(
        result.returndata.as_ref(),
        expected.as_slice(),
        "sub-call returndata must equal the outbe Poseidon precompile output",
    );
    assert_eq!(result.returndata.len(), 32, "poseidon output is 32 bytes");

    assert!(
        result.gas_used > 0 && result.gas_used < 1_000_000,
        "gas_used must be within the requested budget, got {}",
        result.gas_used,
    );
}

#[test]
fn subcall_reaches_nod_with_the_same_runtime_body_readers() {
    let adapter = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = adapter.clone();
    let writer: StorageWriterHandle = adapter;
    let readers = RuntimeBodyReaders::new(reader.clone());
    let owner = Address::repeat_byte(0x11);
    let day = WorldwideDay::new(20_260_715);
    let nod_id = outbe_nod::NodContract::generate_nod_id(owner, day).unwrap();
    let bucket_key = B256::repeat_byte(0x42);
    let repository = NodRepositoryWriter::new(reader, writer);
    repository
        .put_bucket(&NodBucketState {
            bucket_key,
            worldwide_day: day,
            floor_price_minor: U256::from(10),
            is_qualified: true,
            total_nods: 1,
            entry_price_minor: U256::from(9),
        })
        .unwrap();
    let item = NodItemState {
        nod_id,
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day: day,
        league_id: 3,
        floor_price_minor: U256::from(10),
        bucket_key,
        cost_amount_minor: U256::from(12),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 1_700_000_000,
    };
    repository.put_nod(&item).unwrap();
    let payload = encode_nod_item_v1(&outbe_nod::canonical_item(&item)).unwrap();
    let commitment =
        body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, nod_id, &payload).unwrap();
    let mut database = CacheDB::new(EmptyDB::default());
    let scope = Arc::new(ExecutionScope::with_parent_tree(
        Arc::new(StaticAuthenticatedParent {
            entity: EntityRef::NodItem(nod_id),
            commitment,
        }),
        CeWorkConfig::new(0, 0, u64::MAX),
    ));
    let block = BlockContext::new(1, 1, outbe_primitives::chain::CHAIN_ID, owner, vec![owner]);
    let mut provider = DirectStorageProvider::new(&mut database, block);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage, scope.as_ref()).unwrap();
    });
    provider.flush().unwrap();

    let calldata = INod::ownerOfCall {
        nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
    }
    .abi_encode();
    let mut ctx = Context::mainnet().with_db(database);

    let result = sub_call::run(
        &mut ctx,
        CALLER,
        false,
        SpecId::PRAGUE,
        Some(readers),
        scope,
        SubCallInput {
            target: NOD_ADDRESS,
            value: U256::ZERO,
            calldata: Bytes::from(calldata),
            gas_limit: 1_000_000,
            is_static: true,
        },
    )
    .expect("repository-backed Nod sub-call must execute");

    assert!(
        matches!(result.status, SubCallStatus::Success),
        "expected Success, got {:?} with returndata 0x{}",
        result.status,
        hex::encode(&result.returndata),
    );
    assert_eq!(
        INod::ownerOfCall::abi_decode_returns(&result.returndata).unwrap(),
        owner
    );
}
