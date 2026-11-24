//! sub-call to an EOA (no bytecode) returns success with
//! empty returndata.
//!
//! evm2's frame dispatch short-circuits when the target's bytecode is
//! empty. It commits the journal checkpoint and returns `InstructionResult::Stop`
//! with empty output. The driver must translate this to
//! `SubCallStatus::Success` + `returndata = empty`.

use alloy_primitives::{Address, Bytes, U256};
use evm2::{
    evm::{CacheDB, EmptyDB},
    SpecId,
};
use outbe_evm::{sub_call, test_support::EvmWorld};
use outbe_primitives::storage::{SubCallInput, SubCallStatus};

const EOA_TARGET: Address = Address::new([0xAB; 20]);
const CALLER: Address = Address::new([0xC0; 20]);

#[test]
fn sub_call_to_eoa_returns_success_empty() {
    let db = CacheDB::new(EmptyDB::default());
    // No insert_account_info(target, ...) - target is an EOA / empty account.
    let host = std::sync::Arc::new(outbe_runtime::OutbeRuntime::new());
    let mut world = EvmWorld::new(db, host.clone());

    let result = sub_call::run_with_host(
        &mut world.evm,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: EOA_TARGET,
            value: U256::ZERO,
            calldata: Bytes::new(),
            gas_limit: 100_000,
            is_static: true,
        },
        host,
    )
    .expect("sub_call to EOA must not fail fatally");

    assert!(
        matches!(result.status, SubCallStatus::Success),
        "expected Success for EOA target, got {:?}",
        result.status
    );
    assert_eq!(
        result.returndata,
        Bytes::new(),
        "EOA call must return empty bytes"
    );
}
