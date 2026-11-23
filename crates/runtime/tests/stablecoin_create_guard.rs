use alloy_consensus::TxLegacy;
use alloy_primitives::{Address, Bytes, TxKind, U256};
use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{AccountInfo, CacheDB, EmptyDB},
    interpreter::InstrStop,
    NoopInspector, SpecId,
};
use outbe_evm::{factory::OutbeEvmTypes, OutbeEvmFactory};
use outbe_system_common::addresses::is_stablecoin_address;
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::Recovered;

use std::sync::LazyLock;

const GAS_LIMIT: u64 = 1_000_000;

static RESERVED_CREATE_CALLER_NONCE_0: LazyLock<Address> =
    LazyLock::new(|| find_create_caller(0, true));
static ADJACENT_CREATE_CALLER_NONCE_0: LazyLock<Address> =
    LazyLock::new(|| find_create_caller(0, false));
static RESERVED_CREATE_CALLER_NONCE_1: LazyLock<Address> =
    LazyLock::new(|| find_create_caller(1, true));

fn test_env() -> EthEvmEnv<OutbeEvmTypes> {
    EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            gas_limit: U256::from(30_000_000_u64),
            ..Default::default()
        },
        1,
    )
}

fn address_from_counter(counter: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[12..].copy_from_slice(&counter.to_be_bytes());
    Address::from(bytes)
}

fn find_create_caller(nonce: u64, reserved: bool) -> Address {
    (1..2_000_000)
        .map(address_from_counter)
        .find(|caller| is_stablecoin_address(caller.create(nonce)) == reserved)
        .expect("search range contains both prefix classes")
}

fn funded_account(nonce: u64) -> AccountInfo {
    AccountInfo {
        balance: U256::from(10u64).pow(U256::from(20u64)),
        nonce,
        ..Default::default()
    }
}

fn create_tx(caller: Address, nonce: u64) -> Recovered<TxEnvelope> {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce,
            gas_price: 0,
            gas_limit: GAS_LIMIT,
            to: TxKind::Create,
            value: U256::ZERO,
            input: Bytes::from_static(&[0x00]),
        }),
        caller,
    )
}

#[test]
fn top_level_create_uses_canonical_collision_while_adjacent_create_succeeds() {
    let reserved_caller = *RESERVED_CREATE_CALLER_NONCE_0;
    let reserved_target = reserved_caller.create(0);
    let adjacent_caller = *ADJACENT_CREATE_CALLER_NONCE_0;
    let adjacent_target = adjacent_caller.create(0);

    let mut reserved_db = CacheDB::new(EmptyDB::default());
    reserved_db.insert_account_info(&reserved_caller, funded_account(0));
    let mut reserved_evm =
        OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
            .create_evm(reserved_db, test_env());
    let reserved = reserved_evm
        .transact(&create_tx(reserved_caller, 0))
        .expect("reserved create executes as an EVM halt")
        .detach();

    assert!(reserved.result.is_halt());
    assert_eq!(reserved.result.stop, InstrStop::CreateCollision);
    assert_eq!(
        reserved
            .pending_state
            .account_changes_for(&reserved_caller)
            .and_then(|acc| acc.current.as_ref().map(|info| info.nonce)),
        Some(1)
    );
    if let Some(dest) = reserved.pending_state.account_changes_for(&reserved_target) {
        if let Some(info) = &dest.current {
            assert_eq!(info.nonce, 0);
            assert_eq!(info.balance, U256::ZERO);
            assert!(info.is_empty_code_hash());
        }
    }

    let mut inspected_db = CacheDB::new(EmptyDB::default());
    inspected_db.insert_account_info(&reserved_caller, funded_account(0));
    let mut inspected_evm =
        OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
            .create_evm(inspected_db, test_env());
    let inspected = inspected_evm
        .transact_with_inspector(
            &create_tx(reserved_caller, 0),
            &mut NoopInspector::default(),
        )
        .expect("inspected reserved create executes as an EVM halt")
        .detach();
    assert!(inspected.result.is_halt());
    assert_eq!(inspected.result.stop, InstrStop::CreateCollision);
    assert_eq!(
        inspected.result.tx_gas_used(),
        reserved.result.tx_gas_used()
    );

    let mut canonical_collision_db = CacheDB::new(EmptyDB::default());
    canonical_collision_db.insert_account_info(&adjacent_caller, funded_account(0));
    canonical_collision_db.insert_account_info(
        &adjacent_target,
        AccountInfo {
            nonce: 1,
            ..Default::default()
        },
    );
    let mut canonical_collision_evm =
        OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
            .create_evm(canonical_collision_db, test_env());
    let canonical_collision = canonical_collision_evm
        .transact(&create_tx(adjacent_caller, 0))
        .expect("ordinary occupied target produces a collision halt")
        .detach();
    assert!(canonical_collision.result.is_halt());
    assert_eq!(canonical_collision.result.stop, InstrStop::CreateCollision);
    assert_eq!(
        canonical_collision.result.tx_gas_used(),
        reserved.result.tx_gas_used()
    );

    let mut adjacent_db = CacheDB::new(EmptyDB::default());
    adjacent_db.insert_account_info(&adjacent_caller, funded_account(0));
    let mut adjacent_evm =
        OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
            .create_evm(adjacent_db, test_env());
    let adjacent = adjacent_evm
        .transact(&create_tx(adjacent_caller, 0))
        .expect("adjacent create executes")
        .detach();

    assert!(adjacent.result.is_success());
    assert_eq!(adjacent.result.created_address, Some(adjacent_target));
    assert_eq!(
        adjacent
            .pending_state
            .account_changes_for(&adjacent_caller)
            .and_then(|acc| acc.current.as_ref().map(|info| info.nonce)),
        Some(1)
    );
    assert!(adjacent
        .pending_state
        .account_changes_for(&adjacent_target)
        .is_some());
}

fn create_and_store_runtime(create2_salt: Option<[u8; 32]>) -> Bytes {
    let mut code = Vec::new();
    if let Some(salt) = create2_salt {
        code.push(0x7f); // PUSH32 salt
        code.extend_from_slice(&salt);
    }
    code.extend_from_slice(&[
        0x60,
        0x01, // PUSH1 init-code length
        0x60,
        0x00, // PUSH1 init-code offset
        0x60,
        0x00, // PUSH1 value
        if create2_salt.is_some() { 0xf5 } else { 0xf0 },
        0x60,
        0x00, // PUSH1 storage slot
        0x55, // SSTORE returned address
        0x00, // STOP
    ]);
    Bytes::from(code)
}

fn execute_parent(parent: Address, nonce: u64, runtime: Bytes) -> (U256, u64) {
    let bytecode = Bytecode::new_raw(runtime);
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &parent,
        AccountInfo {
            nonce,
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..funded_account(nonce)
        },
    );
    let caller = Address::repeat_byte(0xee);
    db.insert_account_info(&caller, funded_account(0));

    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: GAS_LIMIT,
            to: TxKind::Call(parent),
            value: U256::ZERO,
            input: Bytes::new(),
        }),
        caller,
    );
    let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(db, test_env());
    let detached = evm.transact(&tx).expect("parent call executes").detach();
    assert!(detached.result.is_success());
    let parent_changes = detached
        .pending_state
        .account_changes_for(&parent)
        .expect("parent account touched");
    let slot_zero = parent_changes
        .storage
        .slots
        .get(&U256::ZERO)
        .map(|change| change.value.current)
        .unwrap_or(U256::ZERO);
    let parent_nonce = parent_changes
        .current
        .as_ref()
        .expect("parent info present")
        .nonce;
    (slot_zero, parent_nonce)
}

#[test]
fn nested_create_and_create2_return_zero_for_reserved_destinations() {
    let create_parent = *RESERVED_CREATE_CALLER_NONCE_1;
    assert_eq!(
        execute_parent(create_parent, 1, create_and_store_runtime(None)),
        (U256::ZERO, 2)
    );

    let create2_parent = Address::repeat_byte(0x42);
    let init_code = [0x00];
    let (salt, destination) = (0u64..2_000_000)
        .find_map(|counter| {
            let mut salt = [0u8; 32];
            salt[24..].copy_from_slice(&counter.to_be_bytes());
            let destination = create2_parent.create2_from_code(salt, init_code);
            is_stablecoin_address(destination).then_some((salt, destination))
        })
        .expect("search range contains reserved CREATE2 destination");
    assert!(is_stablecoin_address(destination));
    assert_eq!(
        execute_parent(create2_parent, 1, create_and_store_runtime(Some(salt))),
        (U256::ZERO, 2)
    );
}

#[test]
fn native_value_transfer_to_reserved_address_remains_valid() {
    let sender = Address::repeat_byte(0x77);
    let reserved = RESERVED_CREATE_CALLER_NONCE_0.create(0);
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&sender, funded_account(0));

    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: 100_000,
            to: TxKind::Call(reserved),
            value: U256::from(123u64),
            input: Bytes::new(),
        }),
        sender,
    );
    let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(db, test_env());
    let detached = evm.transact(&tx).expect("transfer executes").detach();

    assert!(detached.result.is_success());
    assert_eq!(
        detached
            .pending_state
            .account_changes_for(&reserved)
            .and_then(|acc| acc.current.as_ref().map(|info| info.balance)),
        Some(U256::from(123u64))
    );
}
