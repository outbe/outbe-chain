//! Wide block timestamps revert before a precompile admits value or mutates state.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{Revert, SolError};
use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    evm::{precompile::PrecompileProvider as _, CacheDB, EmptyDB},
    interpreter::{GasTracker, Message, MessageExt, MessageKind},
    precompiles::{PrecompileError as EvmPrecompileError, PrecompileHalt},
    SpecId,
};
use outbe_evm::{
    factory::{OutbeEvm, OutbeEvmTypes},
    precompiles::{
        OutbePrecompileExecutionContext, OutbePrecompileProvider, OutbePrecompileRuntime,
    },
    EvmRuntimeHost, OutbeEvmFactory,
};
use outbe_primitives::{storage::gas::PRECOMPILE_BASE_GAS, system_tx::SystemTx};
use outbe_system_common::addresses::{
    GRATIS_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS, STAKING_ADDRESS, SYSTEM_ADDRESS,
};
use reth_ethereum::evm::EthEvmEnv;
use std::sync::Arc;

fn host() -> Arc<outbe_runtime::OutbeRuntime> {
    Arc::new(outbe_runtime::OutbeRuntime::new())
}

fn precompiles() -> OutbePrecompileProvider {
    OutbePrecompileProvider::new(
        OutbePrecompileExecutionContext::new(SpecId::PRAGUE, B256::ZERO),
        OutbePrecompileRuntime::new(),
        host(),
    )
}

fn evm_with_timestamp(timestamp: U256) -> OutbeEvm<'static> {
    let env = EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            number: U256::from(1),
            timestamp,
            ..Default::default()
        },
        1,
    );
    OutbeEvmFactory::new(host()).create_evm(CacheDB::new(EmptyDB::default()), env)
}

fn message(address: Address, value: U256, gas_limit: u64, data: Bytes) -> Message<OutbeEvmTypes> {
    MessageExt {
        kind: MessageKind::Call,
        depth: 0,
        gas_limit,
        reservoir: 0,
        destination: address,
        call_target: address,
        caller: SYSTEM_ADDRESS,
        input: data.into(),
        value,
        code: Bytecode::default(),
        code_address: address,
        caller_is_static: false,
        salt: B256::ZERO,
        ext: (),
        _non_exhaustive: (),
    }
}

#[test]
fn wide_timestamp_dispatch_reverts_before_value_admission_or_mutation_authority() {
    let cycle_data = SystemTx::BeginBlock { step_index: 8 }.encode().unwrap();
    for timestamp in [U256::from(u64::MAX) + U256::from(1), U256::MAX] {
        for (address, value, data) in [
            (GRATIS_ADDRESS, U256::from(7), Bytes::new()),
            (STAKING_ADDRESS, U256::from(7), Bytes::new()),
            (OUTBE_SYSTEM_TX_ADDRESS, U256::ZERO, cycle_data.clone()),
        ] {
            let mut evm = evm_with_timestamp(timestamp);
            let runtime = host();
            let base_gas = runtime
                .precompile_base_gas(&address, data.as_ref())
                .max(PRECOMPILE_BASE_GAS);
            let gas_limit = base_gas + 100_000;
            let call = message(address, value, gas_limit, data);
            let mut gas = GasTracker::new(gas_limit);
            let output = precompiles()
                .execute(&mut evm, &call, &mut gas)
                .expect("registered route");
            assert!(
                matches!(
                    output,
                    Err(EvmPrecompileError::Revert(ref bytes))
                        if bytes.as_ref() == Revert::from("timestamp exceeds u64").abi_encode()
                ),
                "expected timestamp revert, got {output:?}"
            );
            assert_eq!(gas.remaining(), gas_limit - base_gas);
            assert!(evm.logs().is_empty());
            assert!(evm.state().journal().is_empty());
            assert_eq!(evm.state().loaded_accounts().count(), 0);
        }
    }
}

#[test]
fn wide_timestamp_dispatch_preserves_out_of_gas_and_unknown_routes() {
    for (address, gas_limit, expect_oog) in
        [(GRATIS_ADDRESS, 0, true), (Address::ZERO, 100_000, false)]
    {
        let mut evm = evm_with_timestamp(U256::MAX);
        let call = message(address, U256::ZERO, gas_limit, Bytes::new());
        let mut gas = GasTracker::new(gas_limit);
        let output = precompiles().execute(&mut evm, &call, &mut gas);
        if expect_oog {
            assert!(
                matches!(
                    output,
                    Some(Err(EvmPrecompileError::Halt(PrecompileHalt::OutOfGas)))
                ),
                "expected OutOfGas halt, got {output:?}"
            );
        } else {
            assert!(
                output.is_none(),
                "expected None for unknown route, got {output:?}"
            );
        }
    }
}
