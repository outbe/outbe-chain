use super::super::{
    extend_outbe_precompiles, OutbePrecompileExecutionContext, OutbePrecompileRuntime,
};
use alloy_evm::precompiles::PrecompilesMap;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{Revert, SolError};
use outbe_primitives::{
    addresses::{GRATIS_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS, STAKING_ADDRESS, SYSTEM_ADDRESS},
    storage::gas::PRECOMPILE_BASE_GAS,
};
use revm::{
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::{EthPrecompiles, MainContext as _, PrecompileProvider},
    interpreter::{CallInput, CallInputs, CallScheme, CallValue, InstructionResult},
    primitives::hardfork::SpecId,
    state::Bytecode,
    Context,
};

fn precompiles() -> PrecompilesMap {
    let mut precompiles =
        PrecompilesMap::from_static(EthPrecompiles::new(SpecId::PRAGUE).precompiles);
    extend_outbe_precompiles::<CacheDB<EmptyDB>>(
        &mut precompiles,
        OutbePrecompileExecutionContext::new(SpecId::PRAGUE, B256::ZERO),
        OutbePrecompileRuntime::new(),
    );
    precompiles
}

fn inputs(address: Address, value: U256, gas_limit: u64, data: Bytes) -> CallInputs {
    CallInputs {
        input: CallInput::Bytes(data),
        return_memory_offset: 0..0,
        gas_limit,
        reservoir: 0,
        bytecode_address: address,
        known_bytecode: (B256::ZERO, Bytecode::default()),
        target_address: address,
        caller: SYSTEM_ADDRESS,
        value: CallValue::Transfer(value),
        scheme: CallScheme::Call,
        is_static: false,
        charged_new_account_state_gas: false,
    }
}

#[test]
fn wide_timestamp_dispatch_reverts_before_value_admission_or_mutation_authority() {
    let cycle_data = crate::system_tx::SystemTxInputV2::CycleTick
        .encode()
        .unwrap();
    for timestamp in [U256::from(u64::MAX) + U256::from(1), U256::MAX] {
        for (address, value, data) in [
            (GRATIS_ADDRESS, U256::from(7), Bytes::new()),
            (STAKING_ADDRESS, U256::from(7), Bytes::new()),
            (OUTBE_SYSTEM_TX_ADDRESS, U256::ZERO, cycle_data.clone()),
        ] {
            let mut ctx = Context::mainnet().with_db(CacheDB::<EmptyDB>::default());
            ctx.block.timestamp = timestamp;
            ctx.block.number = U256::from(1);
            let journal_before = ctx.journaled_state.inner.clone();
            let base_gas = crate::precompile_routes::resolve(&address)
                .unwrap()
                .base_gas(data.as_ref())
                .max(PRECOMPILE_BASE_GAS);
            let call = inputs(address, value, base_gas + 100_000, data);
            let output = precompiles().run(&mut ctx, &call).unwrap().unwrap();
            assert_eq!(output.result, InstructionResult::Revert);
            assert_eq!(
                output.output.as_ref(),
                Revert::from("timestamp exceeds u64").abi_encode()
            );
            assert_eq!(output.gas.remaining(), call.gas_limit - base_gas);
            assert_eq!(ctx.journaled_state.inner, journal_before);
            assert!(ctx.journaled_state.logs().is_empty());
            assert!(ctx.journaled_state.database.cache.accounts.is_empty());
        }
    }
}

#[test]
fn wide_timestamp_dispatch_preserves_out_of_gas_and_unknown_routes() {
    for (address, gas_limit, expected) in [
        (GRATIS_ADDRESS, 0, Some(InstructionResult::PrecompileOOG)),
        (Address::ZERO, 100_000, None),
    ] {
        let mut ctx = Context::mainnet().with_db(CacheDB::<EmptyDB>::default());
        ctx.block.timestamp = U256::MAX;
        let call = inputs(address, U256::ZERO, gas_limit, Bytes::new());
        let output = precompiles().run(&mut ctx, &call).unwrap();
        assert_eq!(output.map(|output| output.result), expected);
    }
}
