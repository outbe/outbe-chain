//! Execution-level coverage for the native-value guard at the outbe precompile
//! boundary.
//!
//! These drive real EVM frames through `OutbeEvmFactory` rather than calling the
//! classifier directly, so they exercise the opcode shapes an attacker actually
//! emits. `CALLCODE` and `DELEGATECALL` reach a precompile with a
//! `bytecode_address` that differs from the `target_address` revm credited: that
//! is how forged value would be booked by a payable precompile that trusts
//! `msg.value`, and how a contract would act under its own caller's identity
//! against the precompile's global state.

use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{keccak256, Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolError};
use outbe_evm::OutbeEvmFactory;
use outbe_primitives::addresses::{
    GRATIS_ADDRESS, ORACLE_ADDRESS, STABLECOIN_ADDRESS_PREFIX, STAKING_ADDRESS,
    VALIDATOR_SET_ADDRESS, ZEROFEE_ADDRESS,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_staking::precompile::IStaking;
use outbe_validatorset::contract::ValidatorSet;
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{
        result::{ExecutionResult, Output, ResultAndState},
        BlockEnv, CfgEnv, TxEnv,
    },
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
};

const CHAIN_ID: u64 = 1;
const EOA: Address = Address::new([0xc0; 20]);
/// Contract that borrows precompile code via `CALLCODE`/`DELEGATECALL`.
const BORROWER: Address = Address::new([0xaa; 20]);
const GAS_LIMIT: u64 = 3_000_000;
const STAKE_VALUE: u64 = 7_000;

fn test_env() -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            gas_limit: 30_000_000,
            ..Default::default()
        },
    }
}

fn funded(balance: u64) -> AccountInfo {
    AccountInfo {
        balance: U256::from(balance),
        ..Default::default()
    }
}

/// Bytecode that copies its calldata into memory and forwards it to `target`
/// through `opcode`, then returns the 32-byte success flag.
///
/// `CALLCODE` (`0xf2`) takes a value operand, which we source from `CALLVALUE`
/// so the frame forwards exactly what the outer transaction sent.
/// `DELEGATECALL` (`0xf4`) takes none - it inherits the frame's value.
fn borrow_code(opcode: u8, target: Address) -> Bytes {
    let mut code = vec![
        0x36, // CALLDATASIZE          size
        0x60, 0x00, // PUSH1 0         offset
        0x60, 0x00, // PUSH1 0         destOffset
        0x37, // CALLDATACOPY
        0x60, 0x00, // PUSH1 0         retLength
        0x60, 0x00, // PUSH1 0         retOffset
        0x36, // CALLDATASIZE          argsLength
        0x60, 0x00, // PUSH1 0         argsOffset
    ];
    if opcode == 0xf2 {
        code.push(0x34); // CALLVALUE   value
    }
    code.push(0x73); // PUSH20         address
    code.extend_from_slice(target.as_slice());
    code.push(0x5a); // GAS
    code.push(opcode);
    code.extend_from_slice(&[
        0x50, // POP                   drop the success flag
        0x3d, // RETURNDATASIZE        size
        0x60, 0x00, // PUSH1 0         offset
        0x60, 0x00, // PUSH1 0         destOffset
        0x3e, // RETURNDATACOPY
        0x3d, // RETURNDATASIZE        size
        0x60, 0x00, // PUSH1 0         offset
        0xf3, // RETURN                bubble the inner frame's returndata up
    ]);
    Bytes::from(code)
}

fn stake_calldata(validator: Address, amount: u64) -> Bytes {
    IStaking::stakeCall {
        validatorAddress: validator,
        amount: U256::from(amount),
    }
    .abi_encode()
    .into()
}

fn revert_reason(result: &ExecutionResult) -> Option<String> {
    let bytes = match result {
        ExecutionResult::Revert { output, .. } => output,
        ExecutionResult::Success {
            output: Output::Call(output),
            ..
        } => output,
        _ => return None,
    };
    alloy_sol_types::Revert::abi_decode(bytes)
        .ok()
        .map(|revert| revert.reason)
}

/// Runs `calldata` against `to` with `value` and returns the whole outcome so
/// callers can assert on post-state as well as the result.
fn run(db: CacheDB<EmptyDB>, to: Address, value: u64, calldata: Bytes) -> ResultAndState {
    let mut evm = OutbeEvmFactory::new().create_evm(db, test_env());
    let tx = TxEnv::builder()
        .caller(EOA)
        .nonce(0)
        .kind(TxKind::Call(to))
        .value(U256::from(value))
        .data(calldata)
        .gas_limit(GAS_LIMIT)
        .build()
        .expect("tx builds");
    evm.transact_raw(tx).expect("transaction executes")
}

fn balance_of(outcome: &ResultAndState, address: Address) -> U256 {
    outcome
        .state
        .get(&address)
        .map(|account| account.info.balance)
        .unwrap_or_default()
}

/// Number of storage slots this transaction actually wrote at `address`. The
/// boundary rejects before any storage provider exists, so a rejected call must
/// leave the precompile's storage untouched - including the stake ledger.
fn storage_writes(outcome: &ResultAndState, address: Address) -> usize {
    outcome
        .state
        .get(&address)
        .map(|account| {
            account
                .storage
                .values()
                .filter(|slot| slot.present_value != slot.original_value)
                .count()
        })
        .unwrap_or(0)
}

fn db_with_borrower(opcode: u8) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(EOA, funded(1_000_000_000));
    let mut borrower = funded(0);
    let code = Bytecode::new_raw(borrow_code(opcode, STAKING_ADDRESS));
    borrower.code_hash = code.hash_slow();
    borrower.code = Some(code);
    db.insert_account_info(BORROWER, borrower);
    db
}

fn db_with_registered_validator(validator: Address) -> CacheDB<EmptyDB> {
    let mut seed = HashMapStorageProvider::new(CHAIN_ID);
    seed.set_block_number(1);
    StorageHandle::enter(&mut seed, |storage| {
        let mut validators = ValidatorSet::new(storage);
        validators.config_owner.write(Address::ZERO).unwrap();
        validators.set_config_max_validators(100).unwrap();
        let mut consensus_pubkey = [0u8; 48];
        consensus_pubkey[..20].copy_from_slice(validator.as_slice());
        validators
            .test_register_validator_without_pop(validator, &consensus_pubkey)
            .unwrap();
    });

    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(validator, funded(1_000_000_000));
    let marker = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        VALIDATOR_SET_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    for ((address, slot), value) in seed.storage {
        db.insert_account_storage(address, slot, value)
            .expect("validator registration storage seeds");
    }
    db
}

/// CALLCODE hands the boundary a `CallValue::Transfer` whose amount came off the
/// stack, but caller and target are the same account, so revm's journal performs a
/// balance check and moves nothing. Crediting it would let `staking.stake` book
/// stake `STAKING_ADDRESS` never received - repeatable at no cost.
#[test]
fn callcode_to_staking_with_value_cannot_credit_stake() {
    // CALLCODE sets `caller` to the executing account, so the borrower stakes to
    // itself and the self-stake gate passes: only the value boundary stands
    // between this frame and a credited stake.
    let outcome = run(
        db_with_borrower(0xf2),
        BORROWER,
        STAKE_VALUE,
        stake_calldata(BORROWER, STAKE_VALUE),
    );

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

/// DELEGATECALL inherits the frame's value for `CALLVALUE` purposes and moves no
/// balance at all.
#[test]
fn delegatecall_to_staking_with_value_cannot_credit_stake() {
    // DELEGATECALL keeps the inherited `caller`, which is the EOA that funded
    // the borrower, so the stake must name the EOA for the self-stake gate to
    // pass and leave the value boundary as the only remaining check.
    let outcome = run(
        db_with_borrower(0xf4),
        BORROWER,
        STAKE_VALUE,
        stake_calldata(EOA, STAKE_VALUE),
    );

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

    // A delegated frame is refused whether or not it carries value: dispatch
    // would otherwise run against the precompile's own storage while `caller`
    // stays the frame's inherited caller.
    assert_eq!(
        revert_reason(&outcome.result).as_deref(),
        Some("outbe precompile: delegated call frame cannot execute a precompile"),
        "a zero-value borrowed frame must also be refused, got {:?}",
        outcome.result
    );
}

/// Gratis has no payable selector, so value sent to it has no accounting entry and
/// no withdrawal path; it must be refused before any state is touched.
#[test]
fn plain_call_with_value_to_non_payable_precompile_is_rejected() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(EOA, funded(1_000_000_000));

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
        stake_calldata(EOA, STAKE_VALUE),
    );

    assert!(
        outcome.result.is_success(),
        "a funded self-stake through a plain CALL must succeed, got {:?}",
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

/// Reserving the `0x53c0...` class must not make native value unspendable there: an
/// empty-calldata send is an ordinary transfer the class dispatch returns from
/// without touching token state.
///
/// This address is unissued, which is the branch that also existed before the
/// class narrowing, so the test guards against over-restricting rather than
/// proving the narrowing. The issued-token half is
/// `precompile_routes::tests::registered_stablecoin_token_refuses_a_plain_native_transfer`.
#[test]
fn native_transfer_to_stablecoin_class_address_succeeds() {
    let mut token = [0u8; 20];
    token[..STABLECOIN_ADDRESS_PREFIX.len()].copy_from_slice(&STABLECOIN_ADDRESS_PREFIX);
    token[19] = 0x01;
    let token = Address::from(token);

    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(EOA, funded(1_000_000_000));

    let mut evm = OutbeEvmFactory::new().create_evm(db, test_env());
    let tx = TxEnv::builder()
        .caller(EOA)
        .nonce(0)
        .kind(TxKind::Call(token))
        .value(U256::from(500u64))
        .data(Bytes::new())
        .gas_limit(GAS_LIMIT)
        .build()
        .expect("tx builds");
    let outcome = evm.transact_raw(tx).expect("transaction executes");

    assert!(
        outcome.result.is_success(),
        "native send to a reserved stablecoin address must succeed, got {:?}",
        outcome.result
    );
    assert_eq!(
        outcome
            .state
            .get(&token)
            .map(|account| account.info.balance)
            .unwrap_or_default(),
        U256::from(500u64),
        "the transferred value must land at the token address"
    );
}

/// The impersonation the delegated-frame refusal closes, independent of value.
///
/// `DELEGATECALL` keeps the frame's inherited caller, and dispatch keys the
/// precompile's storage on the borrowed address, so before the refusal a
/// contract could run any caller-authenticated selector - here `unstake` - as
/// whoever called it, against real staking state. Without the guard this reaches
/// staking and fails on staking's own accounting; with it, the frame never runs.
#[test]
fn delegatecall_cannot_act_under_the_inherited_caller() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(EOA, funded(1_000_000_000));
    let mut borrower = funded(0);
    let code = Bytecode::new_raw(borrow_code(0xf4, STAKING_ADDRESS));
    borrower.code_hash = code.hash_slow();
    borrower.code = Some(code);
    db.insert_account_info(BORROWER, borrower);

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
///
/// Oracle put its value check on mutating selectors only, so a funded call to a
/// view like `getPairCount` used to **succeed**: revm had already credited the
/// precompile account, the view ignored the value, and the frame committed. The
/// value then sat at an address with no accounting entry and no way out. The
/// boundary now refuses it before dispatch, and the revert returns the funds.
#[test]
fn funded_call_to_a_reject_route_view_no_longer_settles() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(EOA, funded(1_000_000_000));

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

use revm::{
    context_interface::ContextSetters,
    handler::{EthFrame, Handler, MainnetHandler},
    ExecuteEvm,
};
fn insert_code(
    db: &mut CacheDB<EmptyDB>,
    address: Address,
    code: Bytecode,
    balance: u64,
    nonce: u64,
) {
    db.insert_account_info(
        address,
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
    db.insert_account_info(EOA, funded(1_000_000_000));
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
    db.insert_account_storage(ZEROFEE_ADDRESS, U256::from(42), U256::from(99))
        .unwrap();
    db
}
fn execute_delegation_call(
    db: CacheDB<EmptyDB>,
    to: Address,
    value: u64,
    data: Bytes,
    inspect: bool,
    normalize: bool,
) -> ResultAndState {
    use revm::inspector::InspectorHandler;
    let tx = TxEnv::builder()
        .caller(EOA)
        .nonce(0)
        .kind(TxKind::Call(to))
        .value(U256::from(value))
        .data(data)
        .gas_price(1)
        .gas_limit(GAS_LIMIT)
        .build()
        .unwrap();
    let mut evm = OutbeEvmFactory::new().create_evm(db, test_env());
    if normalize {
        evm.set_inspector_enabled(inspect);
        evm.transact_raw(tx).unwrap()
    } else {
        // Unmodified Revm is the independent empty-code/gas baseline.
        let mut raw = evm.into_inner();
        raw.ctx.set_tx(tx);
        let mut handler: MainnetHandler<
            _,
            revm::context::result::EVMError<std::convert::Infallible>,
            EthFrame,
        > = Default::default();
        let output = if inspect {
            handler.inspect_run(&mut raw)
        } else {
            handler.run(&mut raw)
        }
        .unwrap();
        ResultAndState::new(output, raw.finalize())
    }
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
        let actual = execute_delegation_call(
            delegation_db(true),
            BORROWER,
            10,
            Bytes::new(),
            inspect,
            true,
        );
        let empty = execute_delegation_call(
            delegation_db(false),
            BORROWER,
            10,
            Bytes::new(),
            inspect,
            false,
        );
        assert_eq!(
            actual.result, empty.result,
            "gas and output must match the normal empty-code path"
        );
        assert_eq!(balance_of(&actual, BORROWER), U256::from(11));
        assert_eq!(balance_of(&actual, EOA), balance_of(&empty, EOA));
        assert_eq!(actual.state[&BORROWER].info.nonce, 2);
        assert_eq!(
            actual.state[&BORROWER].info.code_hash,
            Bytecode::new_eip7702(ZEROFEE_ADDRESS).hash_slow()
        );
        assert_eq!(
            actual.state[&ZEROFEE_ADDRESS].info.code_hash,
            Bytecode::new_legacy([0xef].into()).hash_slow()
        );
        assert_eq!(storage_writes(&actual, ZEROFEE_ADDRESS), 0);
        eprintln!(
            "normalization_control inspect={inspect} case=transfer_gas_marker_identity passed"
        );

        for opcode in [0xf1, 0xf2, 0xf4, 0xfa] {
            let mut db = delegation_db(true);
            insert_code(
                &mut db,
                forwarder,
                forward_call(BORROWER, opcode, false),
                0,
                0,
            );
            let actual = execute_delegation_call(db, forwarder, 10, Bytes::new(), inspect, true);
            let mut baseline_db = delegation_db(false);
            insert_code(
                &mut baseline_db,
                forwarder,
                forward_call(BORROWER, opcode, false),
                0,
                0,
            );
            let baseline =
                execute_delegation_call(baseline_db, forwarder, 10, Bytes::new(), inspect, false);
            assert_eq!(
                actual.result, baseline.result,
                "nested CALL result and gas must match empty delegation: opcode={opcode:x}"
            );
            assert_eq!(balance_of(&actual, EOA), balance_of(&baseline, EOA));
            match &actual.result {
                ExecutionResult::Success {
                    output: Output::Call(data),
                    ..
                } => assert_eq!(U256::from_be_slice(data), U256::from(1)),
                other => panic!("call scheme {opcode:x}: {other:?}"),
            }
            assert_eq!(
                balance_of(&actual, BORROWER),
                U256::from(if opcode == 0xf1 { 11 } else { 1 })
            );
            assert_eq!(storage_writes(&actual, ZEROFEE_ADDRESS), 0);
            eprintln!("normalization_control inspect={inspect} case=opcode_{opcode:x} passed");
        }
        let mut db = delegation_db(true);
        insert_code(&mut db, forwarder, forward_call(BORROWER, 0xf1, true), 0, 0);
        let actual = execute_delegation_call(db, forwarder, 10, Bytes::new(), inspect, true);
        assert!(matches!(actual.result, ExecutionResult::Revert { .. }));
        assert_eq!(balance_of(&actual, BORROWER), U256::from(1));
        assert_eq!(balance_of(&actual, forwarder), U256::ZERO);
        eprintln!("normalization_control inspect={inspect} case=outer_revert passed");

        let mut db = delegation_db(true);
        insert_code(&mut db, BORROWER, Bytecode::new_eip7702(ordinary), 1, 2);
        insert_code(
            &mut db,
            ordinary,
            Bytecode::new_legacy(vec![0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3].into()),
            0,
            0,
        );
        let baseline =
            execute_delegation_call(db.clone(), BORROWER, 10, Bytes::new(), inspect, false);
        let actual = execute_delegation_call(db, BORROWER, 10, Bytes::new(), inspect, true);
        assert_eq!(actual.result, baseline.result);
        match actual.result {
            ExecutionResult::Success {
                output: Output::Call(data),
                ..
            } => assert_eq!(U256::from_be_slice(&data), U256::from(42)),
            other => panic!("ordinary delegation: {other:?}"),
        }
        eprintln!("normalization_control inspect={inspect} case=ordinary_delegation passed");

        let mut db = delegation_db(true);
        insert_code(&mut db, BORROWER, Bytecode::new_eip7702(ordinary), 1, 2);
        insert_code(
            &mut db,
            ordinary,
            Bytecode::new_eip7702(ZEROFEE_ADDRESS),
            0,
            0,
        );
        let baseline =
            execute_delegation_call(db.clone(), BORROWER, 10, Bytes::new(), inspect, false);
        let actual = execute_delegation_call(db, BORROWER, 10, Bytes::new(), inspect, true);
        assert_eq!(actual.result, baseline.result);
        assert!(format!("{:?}", actual.result).contains("OpcodeNotFound"));
        eprintln!("normalization_control inspect={inspect} case=one_hop_only passed");

        let identity = Address::with_last_byte(4);
        let baseline = execute_delegation_call(
            delegation_db(true),
            identity,
            0,
            Bytes::from_static(b"identity"),
            inspect,
            false,
        );
        let actual = execute_delegation_call(
            delegation_db(true),
            identity,
            0,
            Bytes::from_static(b"identity"),
            inspect,
            true,
        );
        assert_eq!(actual.result, baseline.result);
        eprintln!("normalization_control inspect={inspect} case=ethereum_precompile passed");

        let baseline = execute_delegation_call(
            delegation_db(true),
            ZEROFEE_ADDRESS,
            10,
            Bytes::new(),
            inspect,
            false,
        );
        let actual = execute_delegation_call(
            delegation_db(true),
            ZEROFEE_ADDRESS,
            10,
            Bytes::new(),
            inspect,
            true,
        );
        assert_eq!(actual.result, baseline.result);
        assert!(matches!(actual.result, ExecutionResult::Revert { .. }));
        eprintln!(
            "normalization_control inspect={inspect} case=direct_native_value_rejection passed"
        );
    }
}

#[test]
fn system_call_to_native_delegation_executes_empty_code() {
    use revm::SystemCallEvm;
    let mut baseline = OutbeEvmFactory::new()
        .create_evm(delegation_db(false), test_env())
        .into_inner();
    let expected = baseline
        .system_call_with_caller(EOA, BORROWER, Bytes::new())
        .unwrap();
    let mut actual = OutbeEvmFactory::new().create_evm(delegation_db(true), test_env());
    let actual = actual
        .transact_system_call(EOA, BORROWER, Bytes::new())
        .unwrap();
    assert!(actual.result.is_success());
    assert_eq!(actual.result, expected.result);
    assert_eq!(
        actual.state[&BORROWER].info.code_hash,
        Bytecode::new_eip7702(ZEROFEE_ADDRESS).hash_slow()
    );
    assert_eq!(storage_writes(&actual, ZEROFEE_ADDRESS), 0);
}

#[test]
fn borrowed_native_subcalls_normalize_initial_and_nested_delegations() {
    use outbe_primitives::storage::{SubCallInput, SubCallStatus};
    use revm::context_interface::{ContextTr, JournalTr};
    for nested in [false, true] {
        for is_static in [false, true] {
            let forwarder = Address::new([0xbb; 20]);
            let mut outcomes = Vec::new();
            for marker in [false, true] {
                let mut db = delegation_db(marker);
                insert_code(
                    &mut db,
                    forwarder,
                    forward_call(BORROWER, if is_static { 0xfa } else { 0xf1 }, false),
                    0,
                    0,
                );
                let mut evm = OutbeEvmFactory::new().create_evm(db, test_env());
                // A native caller is already loaded by its enclosing frame.
                evm.ctx_mut()
                    .journal_mut()
                    .load_account_with_code(EOA)
                    .unwrap();
                let outcome = outbe_evm::sub_call::run(
                    evm.ctx_mut(),
                    EOA,
                    false,
                    SpecId::PRAGUE,
                    None,
                    std::sync::Arc::new(outbe_compressed_entities::ExecutionScope::new()),
                    SubCallInput {
                        target: if nested { forwarder } else { BORROWER },
                        value: U256::from(if is_static { 0 } else { 10 }),
                        calldata: Bytes::new(),
                        gas_limit: 100_000,
                        is_static,
                    },
                )
                .unwrap();
                assert!(matches!(outcome.status, SubCallStatus::Success));
                assert_eq!(
                    evm.ctx_mut()
                        .journal_mut()
                        .load_account_with_code(BORROWER)
                        .unwrap()
                        .info
                        .balance,
                    U256::from(if is_static { 1 } else { 11 })
                );
                outcomes.push((outcome.gas_used, outcome.gas_refunded, outcome.returndata));
            }
            assert_eq!(
                outcomes[0], outcomes[1],
                "borrowed call accounting must match empty code"
            );
        }
    }
}
