//! STATICCALL to a contract that attempts SSTORE halts with
//! `StateChangeDuringStaticCall`.
//!
//! Bytecode that tries SSTORE inside STATIC context:
//! ```text
//! 60 01      PUSH1 0x01     ; value
//! 60 00      PUSH1 0x00     ; slot 0
//! 55         SSTORE         ; halts: state change in STATIC
//! ```

use alloy_primitives::{Address, Bytes, U256};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, CacheDB, EmptyDB},
    SpecId,
};
use outbe_evm::{sub_call, test_support::EvmWorld};
use outbe_primitives::storage::{SubCallError, SubCallInput, SubCallStatus};

const TARGET: Address = Address::new([0xAB; 20]);
const CALLER: Address = Address::new([0xC0; 20]);

const SSTORE_BYTECODE: &[u8] = &[
    0x60, 0x01, // PUSH1 0x01
    0x60, 0x00, // PUSH1 0x00
    0x55, // SSTORE
];

#[test]
fn staticcall_attempting_sstore_halts() {
    let mut db = CacheDB::new(EmptyDB::default());
    let code = Bytecode::new_raw(Bytes::from_static(SSTORE_BYTECODE));
    let info = AccountInfo {
        balance: U256::ZERO,
        nonce: 0,
        code_hash: code.hash_slow(),
        code: Some(code),
        ..Default::default()
    };
    db.insert_account_info(&TARGET, info);
    let host = std::sync::Arc::new(outbe_runtime::OutbeRuntime::new());
    let mut world = EvmWorld::new(db, host.clone());

    let result = sub_call::run_with_host(
        &mut world.evm,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: TARGET,
            value: U256::ZERO,
            calldata: Bytes::new(),
            gas_limit: 100_000,
            is_static: true,
        },
        host,
    )
    .expect("sub_call should not fail fatally on a child halt");

    match result.status {
        SubCallStatus::Halt(SubCallError::StateChangeDuringStaticCall) => { /* expected */ }
        other => panic!("expected Halt(StateChangeDuringStaticCall), got {other:?}"),
    }
}
