//! Test-utils copy of the domain precompile table.
//!
//! Production dispatch lives in the runtime crate. EVM tests still build
//! `DomainEvmHost` until that host moves out of this crate.

use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::addresses::*;
use outbe_primitives::error::Result;
use outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS;
use outbe_primitives::storage::StorageHandle;

use crate::precompiles::routes::ValuePolicy;

#[cfg(feature = "test-utils")]
pub(crate) type DispatchFn = fn(StorageHandle, &[u8], Address, U256) -> Result<Bytes>;
#[cfg(feature = "test-utils")]
pub(crate) type BaseGasFn = fn(&[u8]) -> u64;

#[cfg(feature = "test-utils")]
const fn policy_is_payable(policy: ValuePolicy) -> bool {
    matches!(policy, ValuePolicy::Payable)
}

#[cfg(feature = "test-utils")]
fn default_base_gas(_input: &[u8]) -> u64 {
    PRECOMPILE_BASE_GAS
}

#[cfg(feature = "test-utils")]
#[derive(Clone, Copy)]
struct DomainExactRoute {
    dispatch: DispatchFn,
    base_gas: BaseGasFn,
    value_policy: ValuePolicy,
    payable_selectors: &'static [[u8; 4]],
}

#[cfg(feature = "test-utils")]
impl DomainExactRoute {
    fn dispatch(
        self,
        storage: StorageHandle,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        outbe_primitives::dispatch::reject_value_unless_payable(
            data,
            self.payable_selectors,
            &value,
        )?;
        (self.dispatch)(storage, data, caller, value)
    }
}

#[cfg(feature = "test-utils")]
fn stablecoin_policy_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_stablecoinpolicy::precompile::dispatch(storage, data, caller, value)
}

#[cfg(feature = "test-utils")]
fn stablecoin_factory_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_stablecoinfactory::precompile::dispatch(storage, data, caller, value)
}

#[cfg(feature = "test-utils")]
fn vote_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_vote::precompile::dispatch_with_handlers(
        storage,
        data,
        caller,
        value,
        outbe_vote::handlers::registry(),
    )
}

#[cfg(feature = "test-utils")]
macro_rules! define_domain_routes {
    ($($address:path => ($dispatch:expr, $gas:expr, $value_policy:expr, $payable_selectors:expr)),+ $(,)?) => {
        $(
            const _: () = assert!(
                policy_is_payable($value_policy) == !$payable_selectors.is_empty(),
                concat!(
                    "route ValuePolicy disagrees with the dispatch module's PAYABLE_SELECTORS for ",
                    stringify!($address),
                ),
            );
        )+

        fn domain_route(address: &Address) -> Option<DomainExactRoute> {
            match *address {
                $(
                    $address => Some(DomainExactRoute {
                        dispatch: $dispatch,
                        base_gas: $gas,
                        value_policy: $value_policy,
                        payable_selectors: $payable_selectors,
                    }),
                )+
                _ => None,
            }
        }
    };
}

#[cfg(feature = "test-utils")]
define_domain_routes! {
    GRATIS_ADDRESS => (outbe_gratis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gratis::precompile::PAYABLE_SELECTORS),
    GRATIS_FACTORY_ADDRESS => (outbe_gratisfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gratisfactory::precompile::PAYABLE_SELECTORS),
    PROMIS_ADDRESS => (outbe_promis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_promis::precompile::PAYABLE_SELECTORS),
    PROMIS_FACTORY_ADDRESS => (outbe_promisfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_promisfactory::precompile::PAYABLE_SELECTORS),
    TRIBUTE_ADDRESS => (outbe_tribute::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_tribute::precompile::PAYABLE_SELECTORS),
    NOD_ADDRESS => (outbe_nod::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_nod::precompile::PAYABLE_SELECTORS),
    NOD_FACTORY_ADDRESS => (outbe_nodfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_nodfactory::precompile::PAYABLE_SELECTORS),
    GEM_ADDRESS => (outbe_gem::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gem::precompile::PAYABLE_SELECTORS),
    GEM_FACTORY_ADDRESS => (outbe_gemfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gemfactory::precompile::PAYABLE_SELECTORS),
    INTEX_ADDRESS => (outbe_intex::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_intex::precompile::PAYABLE_SELECTORS),
    INTEX_FACTORY_ADDRESS => (outbe_intexfactory::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_intexfactory::precompile::PAYABLE_SELECTORS),
    DESIS_ADDRESS => (outbe_desis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_desis::precompile::PAYABLE_SELECTORS),
    VAULT_ROUTER_ADDRESS => (outbe_vaultrouter::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_vaultrouter::precompile::PAYABLE_SELECTORS),
    CREDIS_ADDRESS => (outbe_credis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_credis::precompile::PAYABLE_SELECTORS),
    CREDIS_FACTORY_ADDRESS => (outbe_credisfactory::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_credisfactory::precompile::PAYABLE_SELECTORS),
    CCA_REGISTRY_ADDRESS => (outbe_ccaregistry::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_ccaregistry::precompile::PAYABLE_SELECTORS),
    TRIBUTE_FACTORY_ADDRESS => (outbe_tributefactory::precompile::dispatch, outbe_tributefactory::precompile::base_gas, ValuePolicy::Reject, outbe_tributefactory::precompile::PAYABLE_SELECTORS),
    VALIDATOR_SET_ADDRESS => (outbe_validatorset::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_validatorset::precompile::PAYABLE_SELECTORS),
    SLASH_INDICATOR_ADDRESS => (outbe_slashindicator::precompile::dispatch, outbe_slashindicator::precompile::base_gas, ValuePolicy::Reject, outbe_slashindicator::precompile::PAYABLE_SELECTORS),
    STAKING_ADDRESS => (outbe_staking::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_staking::precompile::PAYABLE_SELECTORS),
    REWARDS_ADDRESS => (outbe_rewards::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_rewards::precompile::PAYABLE_SELECTORS),
    AGENT_REWARD_ADDRESS => (outbe_agentreward::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_agentreward::precompile::PAYABLE_SELECTORS),
    EMISSION_ADDRESS => (outbe_emission::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_emission::precompile::PAYABLE_SELECTORS),
    METADOSIS_ADDRESS => (outbe_metadosis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_metadosis::precompile::PAYABLE_SELECTORS),
    FIDELITY_ADDRESS => (outbe_fidelity::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_fidelity::precompile::PAYABLE_SELECTORS),
    PROMIS_LIMIT_ADDRESS => (outbe_promislimit::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_promislimit::precompile::PAYABLE_SELECTORS),
    ORACLE_ADDRESS => (outbe_oracle::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_oracle::precompile::PAYABLE_SELECTORS),
    ZEROFEE_ADDRESS => (outbe_zerofee::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_zerofee::precompile::PAYABLE_SELECTORS),
    OUTBE_SYSTEM_TX_ADDRESS => (crate::sys_tx_precompile::dispatch_system_tx_route, default_base_gas, ValuePolicy::Reject, crate::sys_tx_precompile::PAYABLE_SELECTORS),
    ZKPROOF_VERIFY_ADDRESS => (outbe_zk_backend::precompile::dispatch_zk_verify, outbe_zk_backend::precompile::zk_verify_base_gas, ValuePolicy::Reject, outbe_zk_backend::precompile::ZK_VERIFY_PAYABLE_SELECTORS),
    TEE_REGISTRY_ADDRESS => (outbe_teeregistry::v1_precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_teeregistry::v1_precompile::PAYABLE_SELECTORS),
    L2_REGISTRY_ADDRESS => (outbe_l2registry::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_l2registry::precompile::PAYABLE_SELECTORS),
    HYPERLANE_CONTROLLER_ADDRESS => (outbe_hyperlanecontroller::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_hyperlanecontroller::precompile::PAYABLE_SELECTORS),
    CROSSCHAIN_BRIDGE_ADDRESS => (outbe_crosschainbridge::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_crosschainbridge::precompile::PAYABLE_SELECTORS),
    STABLECOIN_FACTORY_ADDRESS => (stablecoin_factory_dispatch, default_base_gas, ValuePolicy::Reject, outbe_stablecoinfactory::precompile::PAYABLE_SELECTORS),
    STABLECOIN_POLICY_REGISTRY_ADDRESS => (stablecoin_policy_dispatch, default_base_gas, ValuePolicy::Reject, outbe_stablecoinpolicy::precompile::PAYABLE_SELECTORS),
    CODE_REGISTRY_ADDRESS => (outbe_coderegistry::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_coderegistry::precompile::PAYABLE_SELECTORS),
    VOTE_ADDRESS => (vote_dispatch, default_base_gas, ValuePolicy::Payable, outbe_vote::precompile::PAYABLE_SELECTORS),
    UPDATE_ADDRESS => (outbe_update::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_update::precompile::PAYABLE_SELECTORS),
}

#[cfg(feature = "test-utils")]
pub(crate) fn dispatch_domain_precompile(
    storage: StorageHandle,
    address: &Address,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    if let Some(route) = domain_route(address) {
        return route.dispatch(storage, data, caller, value);
    }
    if is_stablecoin_address(*address) {
        return outbe_stablecoinfactory::precompile::stablecoin_class_dispatch(
            storage, *address, data, caller, value,
        );
    }
    Err(outbe_primitives::error::PrecompileError::Fatal(
        "precompile route is not installed".into(),
    ))
}

#[cfg(feature = "test-utils")]
pub(crate) fn domain_is_payable(address: &Address) -> bool {
    if let Some(route) = domain_route(address) {
        return policy_is_payable(route.value_policy);
    }
    is_stablecoin_address(*address)
}

#[cfg(feature = "test-utils")]
pub(crate) fn domain_base_gas(address: &Address, input: &[u8]) -> u64 {
    if let Some(route) = domain_route(address) {
        return (route.base_gas)(input);
    }
    if is_stablecoin_address(*address) {
        return outbe_primitives::stablecoin_fork::STABLECOIN_V1_GAS.dispatch_base;
    }
    PRECOMPILE_BASE_GAS
}
