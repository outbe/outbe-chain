//! happy-path sub-call integration test.
//!
//! Deploys minimal EVM bytecode at a target address inside an
//! `EthEvmContext<InMemoryDB>` and invokes `sub_call::run` directly with a
//! `SubCallInput`. Asserts that the driver:
//! 1. Builds a borrowed-ctx Evm (`Evm<&mut EthEvmContext<DB>, ...>`),
//! 2. Loads the target's bytecode through the journal,
//! 3. Runs the frame loop to completion via evm2's canonical
//!    `frame_init` / `frame_run` / `frame_return_result` pattern,
//! 4. Returns `SubCallOutput { status: Success, returndata, ... }` with
//!    the bytes produced by the child contract's `RETURN` opcode.
//!
//! The "contract" is a 10-byte EVM bytecode sequence that pushes
//! `0x000...001` onto memory and returns its 32-byte word:
//!
//! ```text
//! 6001         PUSH1 0x01
//! 6000         PUSH1 0x00
//! 52           MSTORE
//! 6020         PUSH1 0x20
//! 6000         PUSH1 0x00
//! f3           RETURN
//! ```

use alloy_primitives::{Address, Bytes, U256};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, CacheDB, EmptyDB},
    SpecId,
};
use outbe_evm::{
    sub_call,
    test_support::{EvmCtx, EvmWorld},
};
use outbe_primitives::storage::{SubCallInput, SubCallStatus};

const TARGET: Address = Address::new([0xAB; 20]);
const CALLER: Address = Address::new([0xC0; 20]);

/// Minimal EVM bytecode: stores `0x000...001` at memory offset 0 and
/// returns the 32-byte word.
const RETURN_ONE_BYTECODE: &[u8] = &[
    0x60, 0x01, // PUSH1 0x01
    0x60, 0x00, // PUSH1 0x00
    0x52, // MSTORE
    0x60, 0x20, // PUSH1 0x20
    0x60, 0x00, // PUSH1 0x00
    0xF3, // RETURN
];

fn build_evm_with_bytecode(host: std::sync::Arc<outbe_runtime::OutbeRuntime>) -> EvmCtx {
    let mut db = CacheDB::new(EmptyDB::default());
    let code = Bytecode::new_raw(Bytes::from_static(RETURN_ONE_BYTECODE));
    let info = AccountInfo {
        balance: U256::ZERO,
        nonce: 0,
        code_hash: code.hash_slow(),
        code: Some(code),
        ..Default::default()
    };
    db.insert_account_info(&TARGET, info);
    EvmWorld::new(db, host).evm
}

#[test]
fn staticcall_returns_contract_returndata() {
    let host = std::sync::Arc::new(outbe_runtime::OutbeRuntime::new());
    let mut evm = build_evm_with_bytecode(host.clone());

    let result = sub_call::run_with_host(
        &mut evm,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: TARGET,
            value: U256::ZERO,
            calldata: Bytes::new(),
            gas_limit: 1_000_000,
            is_static: true,
        },
        host,
    )
    .expect("sub_call should not return a fatal error");

    assert!(
        matches!(result.status, SubCallStatus::Success),
        "expected Success, got {:?}",
        result.status,
    );

    // Bytecode returns 32 bytes with value 0x00..01 - verify byte-exact.
    let expected = {
        let mut buf = [0u8; 32];
        buf[31] = 1;
        Bytes::from(buf.to_vec())
    };
    assert_eq!(
        result.returndata, expected,
        "returndata must be the 32-byte word the contract RETURNs"
    );

    assert!(
        result.gas_used > 0 && result.gas_used < 1_000_000,
        "gas_used must be within the requested budget, got {}",
        result.gas_used
    );
}
