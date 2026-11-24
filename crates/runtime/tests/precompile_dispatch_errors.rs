//! User-controlled call envelopes must settle inside the EVM, so the next
//! transaction can execute even when a precompile rejects the current call.
use alloy_consensus::TxLegacy;
use alloy_primitives::{keccak256, Address, Bytes, TxKind, U256};
use alloy_sol_types::SolCall;
use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{AccountInfo, CacheDB, EmptyDB},
    SpecId, TxResult,
};
use outbe_ccaregistry::precompile::ICcaRegistry;
use outbe_evm::{factory::OutbeEvmTypes, OutbeEvmFactory};
use outbe_primitives::{storage::gas::PRECOMPILE_BASE_GAS, system_tx::SystemTx};
use outbe_system_common::addresses::{
    CCA_REGISTRY_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS, STAKING_ADDRESS, SYSTEM_ADDRESS,
};
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::Recovered;

const SENDER: Address = Address::repeat_byte(0xc0);
const PROXY: Address = Address::repeat_byte(0xab);

fn env() -> EthEvmEnv<OutbeEvmTypes> {
    EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            number: U256::from(1),
            gas_limit: U256::from(30_000_000_u64),
            ..Default::default()
        },
        1,
    )
}

fn funded_db() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &SENDER,
        AccountInfo {
            balance: U256::from(10_u64.pow(18)),
            ..Default::default()
        },
    );
    db
}

fn funded_system_db() -> CacheDB<EmptyDB> {
    let mut db = funded_db();
    db.insert_account_info(
        &SYSTEM_ADDRESS,
        AccountInfo {
            balance: U256::from(10_u64.pow(18)),
            ..Default::default()
        },
    );
    db
}

fn install(db: &mut CacheDB<EmptyDB>, address: Address, bytes: Bytes) {
    let code = Bytecode::new_raw(bytes);
    db.insert_account_info(
        &address,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
}

/// Returns the child success word followed by its exact returndata. Keeping
/// both lets the test distinguish a caught revert from an empty-account call.
fn proxy(target: Address, opcode: u8, gas: u32) -> Bytes {
    let mut code = vec![
        0x36, 0x60, 0, 0x60, 0, 0x37, 0x60, 0, 0x60, 0, 0x36, 0x60, 0,
    ];
    if opcode == 0xf1 {
        code.extend_from_slice(&[0x60, 0]);
    }
    code.push(0x73);
    code.extend_from_slice(target.as_slice());
    code.push(0x63);
    code.extend_from_slice(&gas.to_be_bytes());
    code.extend_from_slice(&[
        opcode, 0x60, 0, 0x52, 0x3d, 0x60, 0, 0x60, 0x20, 0x3e, 0x3d, 0x60, 0x20, 0x01, 0x60, 0,
        0xf3,
    ]);
    code.into()
}

fn transact_from(
    db: CacheDB<EmptyDB>,
    caller: Address,
    to: Address,
    data: Bytes,
    gas: u64,
) -> TxResult<OutbeEvmTypes> {
    let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(db, env());
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: gas,
            to: TxKind::Call(to),
            value: U256::ZERO,
            input: data,
        }),
        caller,
    );
    evm.transact(&tx)
        .expect("a caller rejection must not abort EVM execution")
        .commit()
}

fn transact(db: CacheDB<EmptyDB>, to: Address, data: Bytes, gas: u64) -> TxResult<OutbeEvmTypes> {
    transact_from(db, SENDER, to, data, gas)
}

#[test]
fn user_cycle_bytes_do_not_abort_direct_or_contract_calls() {
    let data = SystemTx::BeginBlock { step_index: 8 }.encode().unwrap();
    assert_eq!(
        data.len(),
        outbe_primitives::system_tx::SYSTEM_CALLDATA_FRAMING_BYTES
    );
    let intrinsic = 21_000
        + data
            .iter()
            .map(|b| if *b == 0 { 4 } else { 16 })
            .sum::<u64>();
    for spare in [0, 50, 99, 100] {
        let result = transact(
            funded_db(),
            STAKING_ADDRESS,
            data.clone(),
            intrinsic + PRECOMPILE_BASE_GAS + spare,
        );
        assert!(result.is_revert(), "{result:?}");

        let mut db = funded_db();
        install(
            &mut db,
            PROXY,
            proxy(STAKING_ADDRESS, 0xf1, (PRECOMPILE_BASE_GAS + spare) as u32),
        );
        let result = transact(db, PROXY, data.clone(), 100_000);
        assert!(result.is_success(), "{result:?}");
        let output = &result.output;
        assert_eq!(U256::from_be_slice(&output[..32]), U256::ZERO);
        assert!(
            output.len() > 32,
            "child rejection must retain its revert reason"
        );
    }
}

#[test]
fn cycle_probe_requires_both_system_callee_and_system_caller() {
    let data = SystemTx::BeginBlock { step_index: 8 }.encode().unwrap();
    let intrinsic = 21_000
        + data
            .iter()
            .map(|byte| if *byte == 0 { 4 } else { 16 })
            .sum::<u64>();
    for spare in [0, 50, 99, 100] {
        let user_at_system_address = transact(
            funded_db(),
            OUTBE_SYSTEM_TX_ADDRESS,
            data.clone(),
            intrinsic + PRECOMPILE_BASE_GAS + spare,
        );
        assert!(
            user_at_system_address.is_revert(),
            "{user_at_system_address:?}"
        );

        let system_at_user_precompile = transact_from(
            funded_system_db(),
            SYSTEM_ADDRESS,
            STAKING_ADDRESS,
            data.clone(),
            intrinsic + PRECOMPILE_BASE_GAS + spare,
        );
        assert!(
            system_at_user_precompile.is_revert(),
            "{system_at_user_precompile:?}"
        );
    }
}

#[test]
fn system_cycle_probe_out_of_gas_is_an_evm_halt() {
    let data = SystemTx::BeginBlock { step_index: 8 }.encode().unwrap();
    let intrinsic = 21_000
        + data
            .iter()
            .map(|byte| if *byte == 0 { 4 } else { 16 })
            .sum::<u64>();
    let result = transact_from(
        funded_system_db(),
        SYSTEM_ADDRESS,
        OUTBE_SYSTEM_TX_ADDRESS,
        data,
        intrinsic + PRECOMPILE_BASE_GAS + 50,
    );
    assert!(result.is_halt(), "{result:?}");
}

#[test]
fn former_debug_address_does_not_invoke_a_calldata_selected_contract() {
    let former_debug = alloy_primitives::address!("000000000000000000000000000000000000f999");
    // Check registration first so a regression fails without executing an
    // unmetered loop through the obsolete adapter.
    assert!(
        !outbe_runtime::precompiles::outbe_exact_precompile_addresses().contains(&former_debug)
    );
    let mut db = funded_db();
    install(&mut db, PROXY, Bytes::from_static(&[0x5b, 0x60, 0, 0x56]));
    let mut data = vec![0; 64];
    data[12..32].copy_from_slice(PROXY.as_slice());
    data[63] = 1;
    let result = transact(db, former_debug, data.into(), 100_000);
    assert!(
        result.is_success() && result.output.is_empty(),
        "{result:?}"
    );
}

/// A real precompile rejection must retain its custom error through a child CALL.
#[test]
fn caller_domain_error_keeps_its_selector_in_direct_and_child_calls() {
    let calldata: Bytes = ICcaRegistry::getCcaCall {
        cca: Address::repeat_byte(0x55),
    }
    .abi_encode()
    .into();
    let expected = &keccak256("NotRegistered()")[..4];
    let direct = transact(funded_db(), CCA_REGISTRY_ADDRESS, calldata.clone(), 100_000);
    assert!(
        direct.is_revert(),
        "expected a caller rejection, got {direct:?}"
    );
    assert_eq!(direct.output.as_ref(), expected);

    let mut db = funded_db();
    install(&mut db, PROXY, proxy(CCA_REGISTRY_ADDRESS, 0xf1, 50_000));
    let child = transact(db, PROXY, calldata, 100_000);
    assert!(
        child.is_success(),
        "the proxy must catch the child rejection, got {child:?}"
    );
    assert_eq!(U256::from_be_slice(&child.output[..32]), U256::ZERO);
    assert_eq!(&child.output[32..], expected);
}
