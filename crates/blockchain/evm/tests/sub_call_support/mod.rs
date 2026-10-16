use alloy_primitives::Address;
use outbe_evm::sub_call::SubCallEnvironment;
use revm::primitives::hardfork::SpecId;

/// Isolated child-call environment without runtime body readers.
pub fn fresh_environment(self_address: Address, spec: SpecId) -> SubCallEnvironment {
    SubCallEnvironment {
        self_address,
        outer_is_static: false,
        spec,
    }
}
