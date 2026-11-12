//! Canonical TeeRegistry V1 ABI shared by the native precompile and host
//! operator tooling. Keeping one generated interface pins selectors and tuple
//! layout across consensus and transaction construction.

use alloy_sol_types::sol;

sol!(
    #![sol(extra_derives(Debug, PartialEq, Eq))]
    "../../../contracts/precompiles/src/ITeeRegistryV1.sol"
);

mod mapping;

crate::define_tee_registry_binding_v1! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct NodeEnclaveBindingV1
}

crate::impl_tee_registry_binding_v1_mapping!(NodeEnclaveBindingV1);

// Derive the absent view from the same field list as present projections.
crate::impl_tee_registry_binding_v1_mapping!(@default_view);
