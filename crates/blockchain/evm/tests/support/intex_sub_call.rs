//! Shared EVM call fixture for Intex settlement integration tests.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, U256};
use outbe_compressed_entities::ExecutionScope;
use outbe_evm::sub_call;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::storage::{SubCallInput, SubCallOutput};
use revm::primitives::hardfork::SpecId;

use super::EvmCtx;

pub(super) struct IntexSubCall<'a> {
    pub ctx: &'a mut EvmCtx,
    pub scope: &'a Arc<ExecutionScope>,
    pub readers: &'a RuntimeBodyReaders,
}

impl IntexSubCall<'_> {
    pub(super) fn call(
        &mut self,
        caller: Address,
        target: Address,
        calldata: Bytes,
        is_static: bool,
    ) -> SubCallOutput {
        sub_call::run(
            self.ctx,
            sub_call::SubCallEnvironment {
                self_address: caller,
                outer_is_static: false,
                spec: SpecId::PRAGUE,
                runtime_body_readers: Some(self.readers.clone()),
                execution_scope: self.scope.clone(),
            },
            SubCallInput {
                target,
                value: U256::ZERO,
                calldata,
                gas_limit: 5_000_000,
                is_static,
            },
        )
        .unwrap()
    }
}
