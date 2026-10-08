use alloy_primitives::Address;
use outbe_compressed_entities::ExecutionScope;
use outbe_evm::sub_call::SubCallEnvironment;
use revm::primitives::hardfork::SpecId;
use std::sync::Arc;

/// Isolated child-call environment without runtime body readers.
pub fn fresh_environment(self_address: Address, spec: SpecId) -> SubCallEnvironment {
    SubCallEnvironment {
        self_address,
        outer_is_static: false,
        spec,
        runtime_body_readers: None,
        execution_scope: Arc::new(ExecutionScope::default()),
    }
}
