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
use outbe_evm::sub_call;
use outbe_nod::{precompile::INod, NodBucketState, NodItemState, NodRepositoryWriter};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_primitives::addresses::{NOD_ADDRESS, ZKPROOF_POSEIDON_ADDRESS};
use outbe_primitives::storage::{SubCallInput, SubCallStatus};
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    Context,
};

const CALLER: Address = Address::new([0xC0; 20]);

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
    let nod_id = U256::from(7);
    let owner = Address::repeat_byte(0x11);
    let bucket_key = B256::repeat_byte(0x42);
    let repository = NodRepositoryWriter::new(reader, writer);
    repository
        .put_bucket(&NodBucketState {
            bucket_key,
            worldwide_day: WorldwideDay::new(20_260_715),
            floor_price_minor: U256::from(10),
            is_qualified: true,
            total_nods: 1,
            entry_price_minor: U256::from(9),
        })
        .unwrap();
    repository
        .put_nod(&NodItemState {
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: WorldwideDay::new(20_260_715),
            league_id: 3,
            floor_price_minor: U256::from(10),
            bucket_key,
            cost_amount_minor: U256::from(12),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_700_000_000,
        })
        .unwrap();
    let calldata = INod::ownerOfCall { nodId: nod_id }.abi_encode();
    let mut ctx = Context::mainnet().with_db(CacheDB::new(EmptyDB::default()));

    let result = sub_call::run(
        &mut ctx,
        CALLER,
        false,
        SpecId::PRAGUE,
        Some(readers),
        SubCallInput {
            target: NOD_ADDRESS,
            value: U256::ZERO,
            calldata: Bytes::from(calldata),
            gas_limit: 1_000_000,
            is_static: true,
        },
    )
    .expect("repository-backed Nod sub-call must execute");

    assert!(matches!(result.status, SubCallStatus::Success));
    assert_eq!(
        INod::ownerOfCall::abi_decode_returns(&result.returndata).unwrap(),
        owner
    );
}
