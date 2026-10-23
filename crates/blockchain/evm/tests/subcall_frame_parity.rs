//! OIP-00165 compares the Outbe driver with the pinned upstream handler.
//! Both paths start at child-frame entry. Neither path charges transaction intrinsic fees or provider admission costs.
//! Existing `subcall_tests` check provider charges, including EIP-150 limits.

use alloy_primitives::{Address, Bytes, TxKind, U256};
use outbe_evm::{
    sub_call,
    test_support::{fresh_environment, install_code, EvmCtx},
};
use outbe_primitives::storage::{SubCallInput, SubCallStatus};
use revm::{
    context::{result::EVMError, TxEnv},
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::{
        execution::create_init_frame, FrameResult, Handler, MainBuilder, MainContext,
        MainnetHandler,
    },
    interpreter::{
        interpreter_action::FrameInit, CallScheme, FrameInput, Gas, InstructionResult, SharedMemory,
    },
    primitives::{constants::CALL_STACK_LIMIT, hardfork::SpecId},
    state::{AccountInfo, Bytecode},
    Context,
};
use std::convert::Infallible;

const CALLER: Address = Address::repeat_byte(0xc0);
const TARGET: Address = Address::repeat_byte(0xab);
const LEAF: Address = Address::repeat_byte(0xac);
const DELEGATE: Address = Address::repeat_byte(0xad);
const GAS_LIMIT: u64 = 500_000;

fn context(target: &[u8], leaf: &[u8], delegated: bool, depth: usize) -> EvmCtx {
    let mut db = CacheDB::new(EmptyDB::default());
    install_code(
        &mut db,
        LEAF,
        &Bytecode::new_raw(Bytes::copy_from_slice(leaf)),
    );
    let code = Bytecode::new_raw(Bytes::copy_from_slice(target));
    install_code(&mut db, if delegated { DELEGATE } else { TARGET }, &code);
    if delegated {
        install_code(&mut db, TARGET, &Bytecode::new_eip7702(DELEGATE));
    }
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::from(100),
            ..Default::default()
        },
    );
    for address in [TARGET, LEAF] {
        db.insert_account_storage(address, U256::ONE, U256::from(7))
            .unwrap();
    }
    let mut ctx = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.set_spec_and_mainnet_gas_params(SpecId::PRAGUE));
    ctx.journaled_state.set_spec_id(SpecId::PRAGUE);
    // The enclosing execution loads the caller before child-frame entry.
    ctx.journaled_state.load_account_mut(CALLER).unwrap();
    while ctx.journaled_state.depth() < depth {
        ctx.journaled_state.checkpoint();
    }
    ctx
}

fn upstream(ctx: &mut EvmCtx, input: &SubCallInput) -> revm::interpreter::CallOutcome {
    ctx.tx = TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(TARGET))
        .value(input.value)
        .data(input.calldata.clone())
        .gas_limit(input.gas_limit)
        .build()
        .unwrap();
    // Use the upstream loader to resolve the recipient and any EIP-7702 delegation.
    let frame = create_init_frame(ctx, Gas::new(input.gas_limit).tracker_mut())
        .unwrap()
        .unwrap();
    let FrameInput::Call(mut call) = frame else {
        unreachable!()
    };
    if input.is_static {
        call.is_static = true;
        call.scheme = CallScheme::StaticCall;
    }
    let first = FrameInit {
        depth: ctx.journaled_state.depth(),
        memory: SharedMemory::new().new_child_context(),
        frame_input: FrameInput::Call(call),
    };
    let mut evm = ctx.clone().build_mainnet();
    let result = MainnetHandler::<_, EVMError<Infallible>, _>::default()
        .run_exec_loop(&mut evm, first)
        .unwrap();
    *ctx = evm.ctx;
    let FrameResult::Call(outcome) = result else {
        panic!("CALL produced CREATE")
    };
    outcome
}

fn compare(
    target: &[u8],
    leaf: &[u8],
    delegated: bool,
    depth: usize,
    value: u64,
) -> (EvmCtx, outbe_primitives::storage::SubCallOutput) {
    let mut outbe = context(target, leaf, delegated, depth);
    let mut reference = outbe.clone();
    let input = SubCallInput {
        target: TARGET,
        value: U256::from(value),
        calldata: Bytes::new(),
        gas_limit: GAS_LIMIT,
        is_static: false,
    };
    let expected = upstream(&mut reference, &input);
    let actual =
        sub_call::run(&mut outbe, fresh_environment(CALLER, SpecId::PRAGUE), input).unwrap();
    assert_eq!(expected.result.result, InstructionResult::Return);
    assert!(
        matches!(actual.status, SubCallStatus::Success),
        "{actual:?}"
    );
    assert_eq!(actual.returndata, expected.result.output);
    assert_eq!(actual.gas_used, GAS_LIMIT - expected.result.gas.remaining());
    assert_eq!(actual.gas_refunded, expected.result.gas.refunded());
    assert_eq!(outbe.journaled_state.state, reference.journaled_state.state);
    assert_eq!(
        outbe.journaled_state.transient_storage,
        reference.journaled_state.transient_storage
    );
    assert_eq!(
        outbe.journaled_state.logs(),
        reference.journaled_state.logs()
    );
    assert_eq!(outbe.journaled_state.depth(), depth);
    assert_eq!(reference.journaled_state.depth(), depth);
    (outbe, actual)
}

/// Return the child status bit and all child returndata.
fn proxy(opcode: u8) -> Vec<u8> {
    let mut code = vec![0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0];
    if opcode == 0xf1 {
        code.extend_from_slice(&[0x60, 0]);
    }
    code.push(0x73);
    code.extend_from_slice(LEAF.as_slice());
    code.extend_from_slice(&[
        0x62, 0x01, 0x86, 0xa0, opcode, 0x60, 0, 0x52, 0x3d, 0x60, 0, 0x60, 0x20, 0x3e, 0x3d, 0x60,
        0x20, 0x01, 0x60, 0, 0xf3,
    ]);
    code
}

/// Clear a nonzero slot to produce a refund. Emit a log. Return or revert with the word 42.
fn leaf(terminal: u8) -> Vec<u8> {
    vec![
        0x60, 0, 0x60, 1, 0x55, 0x60, 42, 0x60, 0, 0x52, 0x60, 9, 0x60, 32, 0x60, 0, 0xa1, 0x60,
        32, 0x60, 0, terminal,
    ]
}

fn word(output: &[u8], offset: usize) -> U256 {
    U256::from_be_slice(&output[offset..offset + 32])
}
fn slot(ctx: &mut EvmCtx, address: Address) -> U256 {
    ctx.journaled_state.sload(address, U256::ONE).unwrap().data
}

#[test]
fn call_success_preserves_state_logs_and_refund() {
    let (mut ctx, out) = compare(&proxy(0xf1), &leaf(0xf3), false, 0, 0);
    assert_eq!(word(&out.returndata, 0), U256::ONE);
    assert_eq!(word(&out.returndata, 32), U256::from(42));
    assert_eq!(slot(&mut ctx, LEAF), U256::ZERO);
    assert_eq!(out.gas_refunded, 4800);
    assert_eq!(ctx.journaled_state.logs().len(), 1);
    assert_eq!(ctx.journaled_state.logs()[0].address, LEAF);
}

#[test]
fn reverting_child_rolls_back_writes_logs_and_refund_but_returns_data() {
    let (mut ctx, out) = compare(&proxy(0xf1), &leaf(0xfd), false, 0, 0);
    assert_eq!(word(&out.returndata, 0), U256::ZERO);
    assert_eq!(word(&out.returndata, 32), U256::from(42));
    assert_eq!(slot(&mut ctx, LEAF), U256::from(7));
    assert_eq!(out.gas_refunded, 0);
    assert!(ctx.journaled_state.logs().is_empty());
}

#[test]
fn staticcall_write_halts_without_state_logs_or_refund() {
    let (mut ctx, out) = compare(&proxy(0xfa), &leaf(0xf3), false, 0, 0);
    assert_eq!(out.returndata.as_ref(), &[0u8; 32]);
    assert_eq!(slot(&mut ctx, LEAF), U256::from(7));
    assert_eq!(out.gas_refunded, 0);
    assert!(ctx.journaled_state.logs().is_empty());
}

#[test]
fn delegated_bytecode_keeps_storage_address_caller_and_value() {
    // Delegate code writes the TARGET slot, not the LEAF slot. It returns ADDRESS, CALLER, and CALLVALUE.
    let code = [
        0x60, 0, 0x60, 1, 0x55, 0x30, 0x60, 0, 0x52, 0x33, 0x60, 32, 0x52, 0x34, 0x60, 64, 0x52,
        0x60, 96, 0x60, 0, 0xf3,
    ];
    let (mut ctx, out) = compare(&proxy(0xf4), &code, true, 0, 3);
    assert_eq!(word(&out.returndata, 0), U256::ONE);
    assert_eq!(
        word(&out.returndata, 32),
        U256::from_be_slice(TARGET.as_slice())
    );
    assert_eq!(
        word(&out.returndata, 64),
        U256::from_be_slice(CALLER.as_slice())
    );
    assert_eq!(word(&out.returndata, 96), U256::from(3));
    assert_eq!(slot(&mut ctx, TARGET), U256::ZERO);
    assert_eq!(slot(&mut ctx, LEAF), U256::from(7));
    assert_eq!(
        ctx.journaled_state.state[&CALLER].info.balance,
        U256::from(97)
    );
    assert_eq!(
        ctx.journaled_state.state[&TARGET].info.balance,
        U256::from(3)
    );
    assert_eq!(out.gas_refunded, 4800);
}

#[test]
fn nested_call_obeys_the_native_depth_boundary() {
    let leaf = [0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];
    for depth in [CALL_STACK_LIMIT as usize - 1, CALL_STACK_LIMIT as usize] {
        let (_, out) = compare(&proxy(0xf1), &leaf, false, depth, 0);
        let allowed = depth < CALL_STACK_LIMIT as usize;
        assert_eq!(
            word(&out.returndata, 0),
            U256::from(u8::from(allowed)),
            "depth {depth}"
        );
        assert_eq!(out.returndata.len(), if allowed { 64 } else { 32 });
        assert_eq!(out.gas_refunded, 0);
    }
}
