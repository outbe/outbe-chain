//! Nested Outbe precompile dispatch, calldata forwarding and memory restoration.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_evm::sub_call;
use outbe_nod::precompile::INod;
use outbe_primitives::addresses::{
    NOD_ADDRESS, STABLECOIN_POLICY_REGISTRY_ADDRESS, UPDATE_ADDRESS,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallStatus},
};
use outbe_stablecoinpolicy::precompile::IStablecoinPolicyRegistry;
use revm::{
    context::{ContextTr as _, LocalContextTr as _},
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};

const CALLER: Address = Address::new([0xC0; 20]);
const POLICY_CALLER: Address = Address::new([0xA7; 20]);

#[test]
fn subcall_reaches_outbe_policy_precompile() {
    // The test inserts no account for the precompile address. The provider
    // dispatches outbe precompiles. Real accounts/bytecode do not back them.
    let mut ctx = Context::mainnet().with_db(CacheDB::new(EmptyDB::default()));

    let calldata = IStablecoinPolicyRegistry::policyExistsCall {
        policyId: U256::from(42),
    }
    .abi_encode()
    .into();

    let result = sub_call::run(
        &mut ctx,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: STABLECOIN_POLICY_REGISTRY_ADDRESS,
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

    assert_eq!(result.returndata.as_ref(), &[0u8; 32]);
    assert_eq!(result.returndata.len(), 32, "ABI bool output is 32 bytes");

    assert!(
        result.gas_used > 0 && result.gas_used < 1_000_000,
        "gas_used must be within the requested budget, got {}",
        result.gas_used,
    );
}

#[test]
fn subcall_reaches_stablecoin_policy_registry_with_canonical_view_output() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_storage(UPDATE_ADDRESS, U256::ZERO, U256::from(2u64))
        .unwrap();
    let mut ctx = Context::mainnet().with_db(db);
    let calldata = IStablecoinPolicyRegistry::policyExistsCall {
        policyId: U256::from(1u64),
    }
    .abi_encode();

    let result = sub_call::run(
        &mut ctx,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: STABLECOIN_POLICY_REGISTRY_ADDRESS,
            value: U256::ZERO,
            calldata: calldata.into(),
            gas_limit: 100_000,
            is_static: true,
        },
    )
    .expect("policy registry sub-call");

    assert!(matches!(result.status, SubCallStatus::Success));
    assert!(
        IStablecoinPolicyRegistry::policyExistsCall::abi_decode_returns(&result.returndata)
            .unwrap()
    );
}

#[test]
fn contract_originated_policy_call_receives_its_calldata() {
    let calldata = IStablecoinPolicyRegistry::policyExistsCall {
        policyId: U256::from(1),
    }
    .abi_encode();
    let mut code = vec![
        0x60, 0x24, // input size: selector and one ABI word
        0x60, 0x00, // calldata offset
        0x60, 0x00, // memory offset
        0x37, // CALLDATACOPY
        0x60, 0x20, // return size
        0x60, 0x40, // return offset
        0x60, 0x24, // input size
        0x60, 0x00, // input offset
        0x60, 0x00, // value
        0x73, // PUSH20 policy address
    ];
    code.extend_from_slice(STABLECOIN_POLICY_REGISTRY_ADDRESS.as_slice());
    code.extend_from_slice(&[
        0x5a, // GAS
        0xf1, // CALL
        0x60, 0x60, // status output offset
        0x52, // MSTORE CALL status
        0x60, 0x40, // return size: ABI bool at 0x40, status at 0x60
        0x60, 0x40, // return offset
        0xf3, // RETURN
    ]);

    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_storage(UPDATE_ADDRESS, U256::ZERO, U256::from(2u64))
        .unwrap();
    let bytecode = Bytecode::new_raw(Bytes::from(code));
    db.insert_account_info(
        POLICY_CALLER,
        AccountInfo {
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..Default::default()
        },
    );
    let mut ctx = Context::mainnet().with_db(db);
    // Outer memory the nested frame must not disturb, and a non-zero checkpoint for its own.
    ctx.local()
        .shared_memory_buffer()
        .borrow_mut()
        .extend_from_slice(&[0xAB; 64]);

    let result = sub_call::run(
        &mut ctx,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: POLICY_CALLER,
            value: U256::ZERO,
            calldata: calldata.into(),
            gas_limit: 100_000,
            is_static: false,
        },
    )
    .expect("outer contract call");

    let mut expected = [0u8; 32];
    expected[31] = 1;

    assert!(matches!(result.status, SubCallStatus::Success));
    assert_eq!(result.returndata.len(), 64);
    assert_eq!(
        &result.returndata[..32],
        expected.as_slice(),
        "the precompile must decode the caller's calldata",
    );
    assert_eq!(result.returndata[63], 1, "inner policy CALL must succeed");
    assert_eq!(
        &ctx.local().shared_memory_buffer().borrow()[..],
        &[0xAB; 64],
        "the sub-call must restore the caller's memory",
    );
}

#[test]
fn subcall_reaches_nod_in_the_same_evm_state() {
    let owner = Address::repeat_byte(0x11);
    let day = WorldwideDay::new(20_260_715);
    let nod_id = outbe_nod::identity::generate_nod_id(owner, day).unwrap();
    let bucket_key = outbe_nod::identity::bucket_key(day, U256::from(9), 978);
    let item = outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: day,
            league_id: 3,
            bucket_key,
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_700_000_000,
        },
        U256::from(9),
    );
    let mut database = CacheDB::new(EmptyDB::default());
    let block = BlockContext::new(1, 1, outbe_primitives::chain::CHAIN_ID, owner, vec![owner]);
    let mut provider = DirectStorageProvider::new(&mut database, block);
    StorageHandle::enter(&mut provider, |storage| {
        outbe_nod::api::add_nod(
            &storage,
            &item,
            U256::from(9),
            outbe_nod::test_support::sealed_terms(&storage, &item, U256::from(9)),
        )
        .unwrap();
    });
    provider.flush().unwrap();

    let calldata = INod::ownerOfCall {
        nodId: nod_id.to_u256(),
    }
    .abi_encode();
    let mut ctx = Context::mainnet().with_db(database);

    let result = sub_call::run(
        &mut ctx,
        sub_call::SubCallEnvironment {
            self_address: CALLER,
            outer_is_static: false,
            spec: SpecId::PRAGUE,
        },
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
