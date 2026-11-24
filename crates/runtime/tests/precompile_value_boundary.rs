//! Execution-level coverage for the native-value guard at the outbe precompile
//! boundary.
//!
//! These drive real EVM frames through `OutbeEvmFactory` rather than calling the
//! classifier directly, so they exercise the opcode shapes an attacker actually
//! emits. `CALLCODE` and `DELEGATECALL` reach a precompile with a
//! `bytecode_address` that differs from the `target_address` the EVM credited. In
//! that shape, a payable precompile that trusts `msg.value` would book forged
//! value. In that shape, a contract would also act under its own caller's
//! identity against the precompile's global state.

#[path = "common/borrowed_precompile.rs"]
mod borrow_code_fixture;
use borrow_code_fixture::borrow_code;

use alloy_consensus::TxLegacy;
use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy_sol_types::{SolCall, SolError};
use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{ethereum_tx_registry, TxEnvelope},
    evm::{AccountInfo, CacheDB, EmptyDB, SystemTx},
    inspector::NoopInspector,
    Evm, ExecutionConfig, SpecId, TxResult, TxResultWithState, Version,
};
use outbe_evm::{
    factory::OutbeEvmTypes,
    precompiles::{
        OutbePrecompileExecutionContext, OutbePrecompileProvider, OutbePrecompileRuntime,
    },
    OutbeEvmFactory,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_staking::precompile::IStaking;
use outbe_system_common::addresses::{
    GRATIS_ADDRESS, ORACLE_ADDRESS, STABLECOIN_ADDRESS_PREFIX, STAKING_ADDRESS,
    VALIDATOR_SET_ADDRESS, ZEROFEE_ADDRESS,
};
use outbe_validatorset::contract::ValidatorSet;
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::Recovered;
use std::sync::Arc;

const CHAIN_ID: u64 = 1;
const EOA: Address = Address::new([0xc0; 20]);
/// Contract that borrows precompile code via `CALLCODE`/`DELEGATECALL`.
const BORROWER: Address = Address::new([0xaa; 20]);
const GAS_LIMIT: u64 = 3_000_000;
const STAKE_VALUE: u64 = 7_000;

fn test_block_env() -> BlockEnvExt<()> {
    BlockEnvExt {
        gas_limit: U256::from(30_000_000_u64),
        ..Default::default()
    }
}

fn test_env() -> EthEvmEnv<OutbeEvmTypes> {
    EthEvmEnv::new(SpecId::PRAGUE, test_block_env(), CHAIN_ID)
}

fn host() -> Arc<outbe_runtime::OutbeRuntime> {
    Arc::new(outbe_runtime::OutbeRuntime::new())
}

fn baseline_evm(db: CacheDB<EmptyDB>) -> Evm<'static, OutbeEvmTypes> {
    let mut version = Version::new(SpecId::PRAGUE);
    version.chain_id = CHAIN_ID;
    let exec_cfg = ExecutionConfig::<OutbeEvmTypes>::for_config::<
        evm2::BaseEvmConfig<{ SpecId::PRAGUE as u32 }>,
    >()
    .with_version(version);
    Evm::new_with_execution_config(
        exec_cfg,
        SpecId::PRAGUE,
        test_block_env(),
        ethereum_tx_registry(SpecId::PRAGUE),
        db,
        OutbePrecompileProvider::new(
            OutbePrecompileExecutionContext::new(SpecId::PRAGUE, B256::ZERO),
            OutbePrecompileRuntime::new(),
            host(),
        ),
    )
}

fn funded(balance: u64) -> AccountInfo {
    AccountInfo {
        balance: U256::from(balance),
        ..Default::default()
    }
}

fn stake_calldata(validator: Address, amount: u64) -> Bytes {
    IStaking::stakeCall {
        validatorAddress: validator,
        amount: U256::from(amount),
        consensusPubkey: Bytes::from(vec![1; 48]),
        radicleNodeId: B256::from([1; 32]),
        blsRegistrationSignature: Bytes::from(vec![1; 96]),
        p2pVersion: 0,
        p2pEncoded: Bytes::new(),
    }
    .abi_encode()
    .into()
}

fn topup_calldata(validator: Address, amount: u64) -> Bytes {
    IStaking::topupCall {
        validatorAddress: validator,
        amount: U256::from(amount),
    }
    .abi_encode()
    .into()
}

fn revert_reason(result: &TxResult<OutbeEvmTypes>) -> Option<String> {
    if !result.is_revert() && !result.is_success() {
        return None;
    }
    alloy_sol_types::Revert::abi_decode(&result.output)
        .ok()
        .map(|revert| revert.reason)
}

fn legacy_tx(to: Address, value: u64, gas_price: u128, calldata: Bytes) -> Recovered<TxEnvelope> {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(CHAIN_ID),
            nonce: 0,
            gas_price,
            gas_limit: GAS_LIMIT,
            to: TxKind::Call(to),
            value: U256::from(value),
            input: calldata,
        }),
        EOA,
    )
}

/// Runs `calldata` against `to` with `value` and returns the whole outcome so
/// callers can assert on post-state as well as the result.
fn run(
    db: CacheDB<EmptyDB>,
    to: Address,
    value: u64,
    calldata: Bytes,
) -> TxResultWithState<OutbeEvmTypes> {
    let mut evm = OutbeEvmFactory::new(host()).create_evm(db, test_env());
    let tx = legacy_tx(to, value, 0, calldata);
    evm.transact(&tx).expect("transaction executes").detach()
}

fn balance_of(outcome: &TxResultWithState<OutbeEvmTypes>, address: Address) -> U256 {
    outcome
        .pending_state
        .account_changes_for(&address)
        .and_then(|account| account.current.map(|info| info.balance))
        .unwrap_or_default()
}

fn account_info_of(outcome: &TxResultWithState<OutbeEvmTypes>, address: Address) -> AccountInfo {
    outcome
        .pending_state
        .account_changes_for(&address)
        .and_then(|account| account.current)
        .expect("account present in pending state")
        .clone()
}

/// Number of storage slots this transaction actually wrote at `address`. The
/// boundary rejects before any storage provider exists, so a rejected call must
/// leave the precompile's storage untouched - including the stake ledger.
fn storage_writes(outcome: &TxResultWithState<OutbeEvmTypes>, address: Address) -> usize {
    outcome
        .pending_state
        .account_changes_for(&address)
        .map(|account| {
            account
                .storage
                .slots
                .values()
                .filter(|slot| slot.value.current != slot.value.original)
                .count()
        })
        .unwrap_or(0)
}

fn db_with_borrower(opcode: u8) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&EOA, funded(1_000_000_000));
    let mut borrower = funded(0);
    let code = Bytecode::new_legacy(borrow_code(opcode, STAKING_ADDRESS));
    borrower.code_hash = code.hash_slow();
    borrower.code = Some(code);
    db.insert_account_info(&BORROWER, borrower);
    db
}

fn db_with_registered_validator(validator: Address) -> CacheDB<EmptyDB> {
    let mut seed = HashMapStorageProvider::new(CHAIN_ID);
    seed.set_block_number(1);
    StorageHandle::enter(&mut seed, |storage| {
        let mut validators = ValidatorSet::new(storage);
        validators.set_config_max_validators(100).unwrap();
        let mut consensus_pubkey = [0u8; 48];
        consensus_pubkey[..20].copy_from_slice(validator.as_slice());
        validators
            .register_validator(validator, &consensus_pubkey)
            .unwrap();
    });

    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&validator, funded(1_000_000_000));
    let marker = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        &VALIDATOR_SET_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    for ((address, slot), value) in seed.storage {
        db.insert_account_storage(&address, &slot, &value);
    }
    db
}

/// CALLCODE hands the boundary a `CallValue::Transfer` whose amount came off the
/// stack. But caller and target are the same account, so the journal performs a
/// balance check and moves nothing. Crediting it would let `staking.stake` book
/// stake `STAKING_ADDRESS` never received - repeatable at no cost.
#[test]
fn callcode_to_staking_with_value_cannot_credit_stake() {
    for calldata in [
        stake_calldata(BORROWER, STAKE_VALUE),
        topup_calldata(BORROWER, STAKE_VALUE),
    ] {
        let outcome = run(db_with_borrower(0xf2), BORROWER, STAKE_VALUE, calldata);

        assert_eq!(
            revert_reason(&outcome.result).as_deref(),
            Some("outbe precompile: delegated call frame cannot execute a precompile"),
            "CALLCODE frame must be rejected at the value boundary, got {:?}",
            outcome.result
        );
        assert_eq!(
            storage_writes(&outcome, STAKING_ADDRESS),
            0,
            "no stake may be booked: the staking ledger must be untouched"
        );
        assert_eq!(
            balance_of(&outcome, STAKING_ADDRESS),
            U256::ZERO,
            "no balance may reach STAKING_ADDRESS through a borrowed frame"
        );
    }
}

/// DELEGATECALL inherits the frame's value for `CALLVALUE` purposes and moves no
/// balance at all.
#[test]
fn delegatecall_to_staking_with_value_cannot_credit_stake() {
    for calldata in [
        stake_calldata(EOA, STAKE_VALUE),
        topup_calldata(EOA, STAKE_VALUE),
    ] {
        let outcome = run(db_with_borrower(0xf4), BORROWER, STAKE_VALUE, calldata);

        assert_eq!(
            revert_reason(&outcome.result).as_deref(),
            Some("outbe precompile: delegated call frame cannot execute a precompile"),
            "DELEGATECALL frame must be rejected at the value boundary, got {:?}",
            outcome.result
        );
        assert_eq!(
            storage_writes(&outcome, STAKING_ADDRESS),
            0,
            "no stake may be booked through an inherited-value frame"
        );
        assert_eq!(balance_of(&outcome, STAKING_ADDRESS), U256::ZERO);
    }
}

/// The refusal is not about value: a borrowed frame carrying nothing is refused
/// as well, because the impersonation is in the frame shape, not the amount.
#[test]
fn zero_value_callcode_is_refused_too() {
    let outcome = run(
        db_with_borrower(0xf2),
        BORROWER,
        0,
        stake_calldata(BORROWER, STAKE_VALUE),
    );

    assert_eq!(
        revert_reason(&outcome.result).as_deref(),
        Some("outbe precompile: delegated call frame cannot execute a precompile"),
        "a zero-value borrowed frame must also be refused, got {:?}",
        outcome.result
    );
}

/// Gratis has no payable selector, so value sent to it has no accounting entry and
/// no withdrawal path. It must be refused before any state is touched.
#[test]
fn plain_call_with_value_to_non_payable_precompile_is_rejected() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&EOA, funded(1_000_000_000));

    let outcome = run(db, GRATIS_ADDRESS, 500, Bytes::new());

    assert_eq!(
        revert_reason(&outcome.result).as_deref(),
        Some("outbe precompile: non-payable address called with value"),
        "value to a non-payable precompile must revert, got {:?}",
        outcome.result
    );
    assert_eq!(
        balance_of(&outcome, GRATIS_ADDRESS),
        U256::ZERO,
        "the rejected value must not settle at the precompile"
    );
}

/// The regression side of the change: a genuine funded `CALL` to a payable
/// precompile must still be credited. Without this, a boundary that rejects
/// everything would look identical to a correct one.
#[test]
fn plain_call_with_value_credits_a_payable_precompile() {
    let db = db_with_registered_validator(EOA);

    let outcome = run(
        db,
        STAKING_ADDRESS,
        STAKE_VALUE,
        topup_calldata(EOA, STAKE_VALUE),
    );

    assert!(
        outcome.result.is_success(),
        "a funded self-topup through a plain CALL must succeed, got {:?}",
        outcome.result
    );
    assert_eq!(
        balance_of(&outcome, STAKING_ADDRESS),
        U256::from(STAKE_VALUE),
        "the staked value must settle at STAKING_ADDRESS"
    );
    assert!(
        storage_writes(&outcome, STAKING_ADDRESS) > 0,
        "the stake must be booked in the staking ledger"
    );
}

/// Reserving the `0x53c0...` class must not make native value unspendable there.
/// An empty-calldata send is an ordinary transfer. The class dispatch returns
/// from it without touching token state.
#[test]
fn native_transfer_to_stablecoin_class_address_succeeds() {
    let mut token = [0u8; 20];
    token[..STABLECOIN_ADDRESS_PREFIX.len()].copy_from_slice(&STABLECOIN_ADDRESS_PREFIX);
    token[19] = 0x01;
    let token = Address::from(token);

    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&EOA, funded(1_000_000_000));

    let outcome = run(db, token, 500, Bytes::new());

    assert!(
        outcome.result.is_success(),
        "native send to a reserved stablecoin address must succeed, got {:?}",
        outcome.result
    );
    assert_eq!(
        balance_of(&outcome, token),
        U256::from(500u64),
        "the transferred value must land at the token address"
    );
}

/// The impersonation the delegated-frame refusal closes, independent of value.
#[test]
fn delegatecall_cannot_act_under_the_inherited_caller() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&EOA, funded(1_000_000_000));
    let mut borrower = funded(0);
    let code = Bytecode::new_legacy(borrow_code(0xf4, STAKING_ADDRESS));
    borrower.code_hash = code.hash_slow();
    borrower.code = Some(code);
    db.insert_account_info(&BORROWER, borrower);

    let unstake = IStaking::unstakeCall {
        amount: U256::from(1u64),
    }
    .abi_encode();

    let outcome = run(db, BORROWER, 0, unstake.into());

    assert_eq!(
        revert_reason(&outcome.result).as_deref(),
        Some("outbe precompile: delegated call frame cannot execute a precompile"),
        "a borrowed frame must not reach a caller-authenticated selector, got {:?}",
        outcome.result
    );
    assert_eq!(
        storage_writes(&outcome, STAKING_ADDRESS),
        0,
        "the refused frame must leave staking state untouched"
    );
}

/// The stranding bug R-02 names, at execution level.
#[test]
fn funded_call_to_a_reject_route_view_no_longer_settles() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&EOA, funded(1_000_000_000));

    let outcome = run(
        db,
        ORACLE_ADDRESS,
        500,
        Bytes::copy_from_slice(&keccak256("getPairCount()")[..4]),
    );

    assert_eq!(
        revert_reason(&outcome.result).as_deref(),
        Some("outbe precompile: non-payable address called with value"),
        "a funded view call must be refused at the boundary, got {:?}",
        outcome.result
    );
    assert_eq!(
        balance_of(&outcome, ORACLE_ADDRESS),
        U256::ZERO,
        "no value may settle at a precompile that has no accounting for it"
    );
}

fn insert_code(
    db: &mut CacheDB<EmptyDB>,
    address: Address,
    code: Bytecode,
    balance: u64,
    nonce: u64,
) {
    db.insert_account_info(
        &address,
        AccountInfo {
            balance: U256::from(balance),
            nonce,
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
}

fn delegation_db(marker: bool) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&EOA, funded(1_000_000_000));
    insert_code(
        &mut db,
        BORROWER,
        Bytecode::new_eip7702(ZEROFEE_ADDRESS),
        1,
        2,
    );
    if marker {
        insert_code(
            &mut db,
            ZEROFEE_ADDRESS,
            Bytecode::new_legacy([0xef].into()),
            0,
            0,
        );
    }
    db.insert_account_storage(&ZEROFEE_ADDRESS, &U256::from(42), &U256::from(99));
    db
}

struct DelegationCall {
    to: Address,
    value: u64,
    data: Bytes,
}

fn execute_delegation_call(
    db: CacheDB<EmptyDB>,
    call: DelegationCall,
    inspect: bool,
    normalize: bool,
) -> TxResultWithState<OutbeEvmTypes> {
    let DelegationCall { to, value, data } = call;
    let tx = legacy_tx(to, value, 1, data);
    let mut evm = if normalize {
        OutbeEvmFactory::new(host()).create_evm(db, test_env())
    } else {
        // Unmodified evm2 opcodes and tx registry are the independent empty-code/gas baseline.
        baseline_evm(db)
    };
    let executed = if inspect {
        evm.transact_with_inspector(&tx, &mut NoopInspector::default())
    } else {
        evm.transact(&tx)
    }
    .unwrap();
    executed.detach()
}

fn forward_call(target: Address, opcode: u8, revert: bool) -> Bytecode {
    let mut code = vec![0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0];
    if opcode == 0xf1 || opcode == 0xf2 {
        code.push(0x34);
    }
    code.push(0x73);
    code.extend_from_slice(target.as_slice());
    code.extend_from_slice(&[0x5a, opcode]);
    if revert {
        code.extend_from_slice(&[0x50, 0x60, 0, 0x60, 0, 0xfd]);
    } else {
        code.extend_from_slice(&[0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    }
    Bytecode::new_legacy(code.into())
}

#[test]
fn execution_code_normalization_controls() {
    let forwarder = Address::new([0xbb; 20]);
    let ordinary = Address::new([0xdd; 20]);
    for inspect in [false, true] {
        assert_native_transfer_identity(inspect);
        assert_nested_delegation_opcodes(inspect, forwarder)
            .expect("delegation result matches independent baseline");
        assert_outer_delegation_revert(inspect, forwarder);
        assert_ordinary_delegation(inspect, ordinary)
            .expect("delegation result matches independent baseline");
        assert_single_delegation_hop(inspect, ordinary);
        assert_ethereum_precompile(inspect);
        assert_direct_native_value_rejection(inspect);
    }
}

fn assert_native_transfer_identity(inspect: bool) {
    let actual = execute_delegation_call(
        delegation_db(true),
        DelegationCall {
            to: BORROWER,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        true,
    );
    let empty = execute_delegation_call(
        delegation_db(false),
        DelegationCall {
            to: BORROWER,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        false,
    );
    assert_eq!(
        actual.result, empty.result,
        "gas and output must match the normal empty-code path"
    );
    assert_eq!(balance_of(&actual, BORROWER), U256::from(11));
    assert_eq!(balance_of(&actual, EOA), balance_of(&empty, EOA));
    assert_eq!(account_info_of(&actual, BORROWER).nonce, 2);
    assert_eq!(
        account_info_of(&actual, BORROWER).code_hash,
        Bytecode::new_eip7702(ZEROFEE_ADDRESS).hash_slow()
    );
    assert_eq!(
        account_info_of(&actual, ZEROFEE_ADDRESS).code_hash,
        Bytecode::new_legacy([0xef].into()).hash_slow()
    );
    assert_eq!(storage_writes(&actual, ZEROFEE_ADDRESS), 0);
}

fn assert_nested_delegation_opcodes(inspect: bool, forwarder: Address) -> eyre::Result<()> {
    for opcode in [0xf1, 0xf2, 0xf4, 0xfa] {
        let mut db = delegation_db(true);
        insert_code(
            &mut db,
            forwarder,
            forward_call(BORROWER, opcode, false),
            0,
            0,
        );
        let actual = execute_delegation_call(
            db,
            DelegationCall {
                to: forwarder,
                value: 10,
                data: Bytes::new(),
            },
            inspect,
            true,
        );
        let mut baseline_db = delegation_db(false);
        insert_code(
            &mut baseline_db,
            forwarder,
            forward_call(BORROWER, opcode, false),
            0,
            0,
        );
        let baseline = execute_delegation_call(
            baseline_db,
            DelegationCall {
                to: forwarder,
                value: 10,
                data: Bytes::new(),
            },
            inspect,
            false,
        );
        assert_eq!(
            actual.result, baseline.result,
            "nested CALL result and gas must match empty delegation: opcode={opcode:x}"
        );
        assert_eq!(balance_of(&actual, EOA), balance_of(&baseline, EOA));
        if !actual.result.is_success() {
            eyre::bail!("call scheme {opcode:x}: {:?}", actual.result);
        }
        assert_eq!(U256::from_be_slice(&actual.result.output), U256::from(1));
        assert_eq!(
            balance_of(&actual, BORROWER),
            U256::from(if opcode == 0xf1 { 11 } else { 1 })
        );
        assert_eq!(storage_writes(&actual, ZEROFEE_ADDRESS), 0);
    }
    Ok(())
}

fn assert_outer_delegation_revert(inspect: bool, forwarder: Address) {
    let mut db = delegation_db(true);
    insert_code(&mut db, forwarder, forward_call(BORROWER, 0xf1, true), 0, 0);
    let actual = execute_delegation_call(
        db,
        DelegationCall {
            to: forwarder,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        true,
    );
    assert!(actual.result.is_revert());
    assert_eq!(balance_of(&actual, BORROWER), U256::from(1));
    assert_eq!(balance_of(&actual, forwarder), U256::ZERO);
}

fn assert_ordinary_delegation(inspect: bool, ordinary: Address) -> eyre::Result<()> {
    let mut db = delegation_db(true);
    insert_code(&mut db, BORROWER, Bytecode::new_eip7702(ordinary), 1, 2);
    insert_code(
        &mut db,
        ordinary,
        Bytecode::new_legacy(vec![0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3].into()),
        0,
        0,
    );
    let baseline = execute_delegation_call(
        db.clone(),
        DelegationCall {
            to: BORROWER,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        false,
    );
    let actual = execute_delegation_call(
        db,
        DelegationCall {
            to: BORROWER,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        true,
    );
    assert_eq!(actual.result, baseline.result);
    if !actual.result.is_success() {
        eyre::bail!("ordinary delegation: {:?}", actual.result);
    }
    assert_eq!(U256::from_be_slice(&actual.result.output), U256::from(42));

    Ok(())
}

fn assert_single_delegation_hop(inspect: bool, ordinary: Address) {
    let mut db = delegation_db(true);
    insert_code(&mut db, BORROWER, Bytecode::new_eip7702(ordinary), 1, 2);
    insert_code(
        &mut db,
        ordinary,
        Bytecode::new_eip7702(ZEROFEE_ADDRESS),
        0,
        0,
    );
    let baseline = execute_delegation_call(
        db.clone(),
        DelegationCall {
            to: BORROWER,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        false,
    );
    let actual = execute_delegation_call(
        db,
        DelegationCall {
            to: BORROWER,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        true,
    );
    assert_eq!(actual.result, baseline.result);
    assert_eq!(
        actual.result.stop,
        evm2::interpreter::InstrStop::OpcodeNotFound,
        "expected OpcodeNotFound halt for second delegation hop, got {:?}",
        actual.result
    );
}

fn assert_ethereum_precompile(inspect: bool) {
    let identity = Address::with_last_byte(4);
    let baseline = execute_delegation_call(
        delegation_db(true),
        DelegationCall {
            to: identity,
            value: 0,
            data: Bytes::from_static(b"identity"),
        },
        inspect,
        false,
    );
    let actual = execute_delegation_call(
        delegation_db(true),
        DelegationCall {
            to: identity,
            value: 0,
            data: Bytes::from_static(b"identity"),
        },
        inspect,
        true,
    );
    assert_eq!(actual.result, baseline.result);
}

fn assert_direct_native_value_rejection(inspect: bool) {
    let baseline = execute_delegation_call(
        delegation_db(true),
        DelegationCall {
            to: ZEROFEE_ADDRESS,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        false,
    );
    let actual = execute_delegation_call(
        delegation_db(true),
        DelegationCall {
            to: ZEROFEE_ADDRESS,
            value: 10,
            data: Bytes::new(),
        },
        inspect,
        true,
    );
    assert_eq!(actual.result, baseline.result);
    assert!(actual.result.is_revert());
}

#[test]
fn system_call_to_native_delegation_executes_empty_code() {
    let mut baseline = baseline_evm(delegation_db(false));
    let expected = baseline
        .system_call(SystemTx::new(BORROWER, Bytes::new()).with_caller(EOA))
        .unwrap()
        .detach();
    let mut actual_evm = OutbeEvmFactory::new(host()).create_evm(delegation_db(true), test_env());
    let actual = outbe_evm::factory::transact_outbe_system_call(
        &mut actual_evm,
        EOA,
        BORROWER,
        Bytes::new(),
    )
    .unwrap();
    assert!(actual.result.is_success());
    assert_eq!(actual.result, expected.result);
    assert_eq!(
        account_info_of(&actual, BORROWER).code_hash,
        Bytecode::new_eip7702(ZEROFEE_ADDRESS).hash_slow()
    );
    assert_eq!(storage_writes(&actual, ZEROFEE_ADDRESS), 0);
}

#[test]
fn borrowed_native_subcalls_normalize_initial_and_nested_delegations() {
    for nested in [false, true] {
        for is_static in [false, true] {
            let forwarder = Address::new([0xbb; 20]);
            let mut outcomes = Vec::new();
            for marker in [false, true] {
                outcomes.push(
                    borrowed_delegation_outcome(nested, is_static, marker, forwarder)
                        .expect("borrowed delegation outcome fixture succeeds"),
                );
            }
            assert_eq!(
                outcomes[0], outcomes[1],
                "borrowed call accounting must match empty code"
            );
        }
    }
}

fn borrowed_delegation_outcome(
    nested: bool,
    is_static: bool,
    marker: bool,
    forwarder: Address,
) -> eyre::Result<(u64, i64, Bytes)> {
    use outbe_primitives::storage::{SubCallInput, SubCallStatus};
    let mut db = delegation_db(marker);
    insert_code(
        &mut db,
        forwarder,
        forward_call(BORROWER, if is_static { 0xfa } else { 0xf1 }, false),
        0,
        0,
    );
    let mut evm = OutbeEvmFactory::new(host()).create_evm(db, test_env());
    // A native caller is already loaded by its enclosing frame.
    evm.state_mut().account(&EOA)?;
    let outcome = outbe_evm::sub_call::run_with_host(
        &mut evm,
        outbe_evm::test_support::fresh_environment(EOA, SpecId::PRAGUE),
        SubCallInput {
            target: if nested { forwarder } else { BORROWER },
            value: U256::from(if is_static { 0 } else { 10 }),
            calldata: Bytes::new(),
            gas_limit: 100_000,
            is_static,
        },
        host(),
    )?;
    assert!(matches!(outcome.status, SubCallStatus::Success));
    assert_eq!(
        evm.state_mut().account(&BORROWER).unwrap().balance(),
        U256::from(if is_static { 1 } else { 11 })
    );
    Ok((outcome.gas_used, outcome.gas_refunded, outcome.returndata))
}
