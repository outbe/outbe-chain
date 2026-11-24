//! OIP-00165 compares the Outbe driver with the pinned upstream handler.
//! Both paths start at child-frame entry. Neither path charges transaction intrinsic fees or provider admission costs.
//! Existing `subcall_tests` check provider charges, including EIP-150 limits.

use alloy_primitives::{Address, Bytes, B256, U256};
use evm2::{
    bytecode::Bytecode,
    constants::CALL_DEPTH_LIMIT,
    evm::{AccountInfo, CacheDB, EmptyDB},
    interpreter::{Host as _, InstrStop, MessageExt, MessageKind, MessageResult},
    BaseEvmTypes, Evm, SpecId,
};
use outbe_evm::{
    sub_call,
    test_support::{call_depth, fresh_environment, install_code, set_call_depth, EvmCtx},
    OutbeEvmFactory,
};
use outbe_primitives::storage::{SubCallInput, SubCallStatus};
use reth_ethereum::evm::EthEvmEnv;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const CALLER: Address = Address::repeat_byte(0xc0);
const TARGET: Address = Address::repeat_byte(0xab);
const LEAF: Address = Address::repeat_byte(0xac);
const DELEGATE: Address = Address::repeat_byte(0xad);
const GAS_LIMIT: u64 = 500_000;

fn build_db(target: &[u8], leaf: &[u8], delegated: bool) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    install_code(
        &mut db,
        LEAF,
        &Bytecode::new_legacy(Bytes::copy_from_slice(leaf)),
    );
    let code = Bytecode::new_legacy(Bytes::copy_from_slice(target));
    install_code(&mut db, if delegated { DELEGATE } else { TARGET }, &code);
    if delegated {
        install_code(&mut db, TARGET, &Bytecode::new_eip7702(DELEGATE));
    }
    db.insert_account_info(
        &CALLER,
        AccountInfo {
            balance: U256::from(100),
            ..Default::default()
        },
    );
    for address in [TARGET, LEAF] {
        db.insert_account_storage(&address, &U256::ONE, &U256::from(7));
    }
    db
}

fn outbe_evm(db: CacheDB<EmptyDB>, depth: u16) -> EvmCtx {
    let host = Arc::new(outbe_runtime::OutbeRuntime::new());
    let env = EthEvmEnv::new(SpecId::PRAGUE, Default::default(), 1);
    let mut evm = OutbeEvmFactory::new(host).create_evm(db, env);
    // The enclosing execution loads the caller before child-frame entry.
    evm.state_mut().account(&CALLER).unwrap();
    set_call_depth(&mut evm, depth);
    evm
}

fn upstream(
    db: CacheDB<EmptyDB>,
    depth: u16,
    input: &SubCallInput,
) -> (Evm<'static, BaseEvmTypes>, MessageResult<BaseEvmTypes>) {
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::PRAGUE,
        Default::default(),
        evm2::ethereum::ethereum_tx_registry(SpecId::PRAGUE),
        db,
        evm2::Precompiles::base(SpecId::PRAGUE),
    );
    evm.state_mut().account(&CALLER).unwrap();
    // Resolve target and single EIP-7702 delegation hop like upstream frame setup.
    let raw_code = evm
        .state_mut()
        .account(&input.target)
        .unwrap()
        .load_code()
        .unwrap();
    let (code_address, code) = if let Some(delegate) = raw_code.eip7702_address() {
        let delegated_code = evm
            .state_mut()
            .account(&delegate)
            .unwrap()
            .load_code()
            .unwrap();
        (delegate, delegated_code)
    } else {
        (input.target, raw_code)
    };
    let mut message = MessageExt {
        kind: if input.is_static {
            MessageKind::StaticCall
        } else {
            MessageKind::Call
        },
        depth: depth + 1,
        gas_limit: input.gas_limit,
        reservoir: 0,
        destination: input.target,
        call_target: input.target,
        caller: CALLER,
        input: input.calldata.clone().into(),
        value: if input.is_static {
            U256::ZERO
        } else {
            input.value
        },
        code,
        code_address,
        caller_is_static: input.is_static,
        salt: B256::ZERO,
        ext: (),
        _non_exhaustive: (),
    };
    let outcome = evm.execute_message(&mut message).unwrap();
    (evm, outcome)
}

fn compare(
    target: &[u8],
    leaf: &[u8],
    delegated: bool,
    depth: u16,
    value: u64,
) -> (EvmCtx, outbe_primitives::storage::SubCallOutput) {
    let db = build_db(target, leaf, delegated);
    let mut outbe = outbe_evm(db.clone(), depth);
    let input = SubCallInput {
        target: TARGET,
        value: U256::from(value),
        calldata: Bytes::new(),
        gas_limit: GAS_LIMIT,
        is_static: false,
    };
    let (mut reference, expected) = upstream(db, depth, &input);
    let actual = sub_call::run_with_host(
        &mut outbe,
        fresh_environment(CALLER, SpecId::PRAGUE),
        input,
        Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .unwrap();
    assert_eq!(expected.stop, InstrStop::Return);
    assert!(
        matches!(actual.status, SubCallStatus::Success),
        "{actual:?}"
    );
    assert_eq!(actual.returndata, expected.output);
    assert_eq!(actual.gas_used, expected.gas.spent());
    assert_eq!(actual.gas_refunded, expected.gas.refunded());
    assert_state_parity(&mut outbe, &mut reference, depth);
    (outbe, actual)
}

fn assert_state_parity(
    outbe: &mut EvmCtx,
    reference: &mut Evm<'_, BaseEvmTypes>,
    expected_depth: u16,
) {
    assert_eq!(
        call_depth(outbe),
        expected_depth,
        "call depth must be restored after sub-call"
    );
    let mut tracked: BTreeMap<Address, BTreeSet<U256>> = BTreeMap::new();
    for address in [CALLER, TARGET, LEAF, DELEGATE] {
        tracked
            .entry(address)
            .or_default()
            .extend([U256::ZERO, U256::ONE]);
    }
    for (address, slots) in outbe.state().loaded_accounts() {
        let entry = tracked.entry(*address).or_default();
        entry.extend([U256::ZERO, U256::ONE]);
        entry.extend(slots.copied());
    }
    for (address, slots) in reference.state().loaded_accounts() {
        let entry = tracked.entry(*address).or_default();
        entry.extend([U256::ZERO, U256::ONE]);
        entry.extend(slots.copied());
    }
    for (address, slots) in tracked {
        let outbe_info = {
            let acc = outbe.state_mut().account(&address).unwrap();
            (acc.balance(), acc.nonce(), acc.code_hash())
        };
        let ref_info = {
            let acc = reference.state_mut().account(&address).unwrap();
            (acc.balance(), acc.nonce(), acc.code_hash())
        };
        assert_eq!(outbe_info, ref_info, "account info mismatch for {address}");
        for key in slots {
            let outbe_val = outbe
                .state_mut()
                .account(&address)
                .unwrap()
                .storage()
                .into_slot(key)
                .unwrap()
                .current();
            let ref_val = reference
                .state_mut()
                .account(&address)
                .unwrap()
                .storage()
                .into_slot(key)
                .unwrap()
                .current();
            assert_eq!(
                outbe_val, ref_val,
                "storage mismatch for {address} slot {key}"
            );
            assert_eq!(
                outbe.state_mut().tload(&address, &key),
                reference.state_mut().tload(&address, &key),
                "transient storage mismatch for {address} slot {key}"
            );
        }
    }
    assert_eq!(outbe.logs(), reference.logs());
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

fn slot(evm: &mut EvmCtx, address: Address) -> U256 {
    evm.state_mut()
        .account(&address)
        .unwrap()
        .storage()
        .into_slot(U256::ONE)
        .unwrap()
        .current()
}

#[test]
fn call_success_preserves_state_logs_and_refund() {
    let (mut evm, out) = compare(&proxy(0xf1), &leaf(0xf3), false, 0, 0);
    assert_eq!(word(&out.returndata, 0), U256::ONE);
    assert_eq!(word(&out.returndata, 32), U256::from(42));
    assert_eq!(slot(&mut evm, LEAF), U256::ZERO);
    assert_eq!(out.gas_refunded, 4800);
    assert_eq!(evm.logs().len(), 1);
    assert_eq!(evm.logs()[0].address, LEAF);
}

#[test]
fn reverting_child_rolls_back_writes_logs_and_refund_but_returns_data() {
    let (mut evm, out) = compare(&proxy(0xf1), &leaf(0xfd), false, 0, 0);
    assert_eq!(word(&out.returndata, 0), U256::ZERO);
    assert_eq!(word(&out.returndata, 32), U256::from(42));
    assert_eq!(slot(&mut evm, LEAF), U256::from(7));
    assert_eq!(out.gas_refunded, 0);
    assert!(evm.logs().is_empty());
}

#[test]
fn staticcall_write_halts_without_state_logs_or_refund() {
    let (mut evm, out) = compare(&proxy(0xfa), &leaf(0xf3), false, 0, 0);
    assert_eq!(out.returndata.as_ref(), &[0u8; 32]);
    assert_eq!(slot(&mut evm, LEAF), U256::from(7));
    assert_eq!(out.gas_refunded, 0);
    assert!(evm.logs().is_empty());
}

#[test]
fn delegated_bytecode_keeps_storage_address_caller_and_value() {
    // Delegate code writes the TARGET slot, not the LEAF slot. It returns ADDRESS, CALLER, and CALLVALUE.
    let code = [
        0x60, 0, 0x60, 1, 0x55, 0x30, 0x60, 0, 0x52, 0x33, 0x60, 32, 0x52, 0x34, 0x60, 64, 0x52,
        0x60, 96, 0x60, 0, 0xf3,
    ];
    let (mut evm, out) = compare(&proxy(0xf4), &code, true, 0, 3);
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
    assert_eq!(slot(&mut evm, TARGET), U256::ZERO);
    assert_eq!(slot(&mut evm, LEAF), U256::from(7));
    assert_eq!(
        evm.state_mut().account(&CALLER).unwrap().balance(),
        U256::from(97)
    );
    assert_eq!(
        evm.state_mut().account(&TARGET).unwrap().balance(),
        U256::from(3)
    );
    assert_eq!(out.gas_refunded, 4800);
}

#[test]
fn nested_call_obeys_the_native_depth_boundary() {
    let leaf = [0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];
    for depth in [CALL_DEPTH_LIMIT - 2, CALL_DEPTH_LIMIT - 1] {
        let (_, out) = compare(&proxy(0xf1), &leaf, false, depth, 0);
        let allowed = depth < CALL_DEPTH_LIMIT - 1;
        assert_eq!(
            word(&out.returndata, 0),
            U256::from(u8::from(allowed)),
            "depth {depth}"
        );
        assert_eq!(out.returndata.len(), if allowed { 64 } else { 32 });
        assert_eq!(out.gas_refunded, 0);
    }
}
