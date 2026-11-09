//! Compact exact-address routing for existing Outbe precompiles.
//!
//! This module owns only execution facts consumed by the dispatcher: exact address,
//! dispatch adapter and base-gas function. Activation, persistence, warming,
//! sponsorship and future address classes intentionally remain outside this table.

use alloy_primitives::{Address, Bytes, U256};
#[cfg(test)]
use outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS;
use outbe_primitives::{addresses::*, error::Result, storage::StorageHandle};

pub(crate) const EXACT_ADDRESSES: &[Address] = &[
    GRATIS_ADDRESS,
    GRATIS_FACTORY_ADDRESS,
    PROMIS_ADDRESS,
    PROMIS_FACTORY_ADDRESS,
    TRIBUTE_ADDRESS,
    NOD_ADDRESS,
    NOD_FACTORY_ADDRESS,
    GEM_ADDRESS,
    GEM_FACTORY_ADDRESS,
    INTEX_ADDRESS,
    INTEX_FACTORY_ADDRESS,
    DESIS_ADDRESS,
    VAULT_ROUTER_ADDRESS,
    CREDIS_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    CCA_REGISTRY_ADDRESS,
    TRIBUTE_FACTORY_ADDRESS,
    VALIDATOR_SET_ADDRESS,
    SLASH_INDICATOR_ADDRESS,
    STAKING_ADDRESS,
    REWARDS_ADDRESS,
    AGENT_REWARD_ADDRESS,
    EMISSION_ADDRESS,
    METADOSIS_ADDRESS,
    FIDELITY_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
    ORACLE_ADDRESS,
    ZEROFEE_ADDRESS,
    OUTBE_SYSTEM_TX_ADDRESS,
    ZKPROOF_VERIFY_ADDRESS,
    TEE_REGISTRY_ADDRESS,
    L2_REGISTRY_ADDRESS,
    HYPERLANE_CONTROLLER_ADDRESS,
    CROSSCHAIN_BRIDGE_ADDRESS,
    STABLECOIN_FACTORY_ADDRESS,
    STABLECOIN_POLICY_REGISTRY_ADDRESS,
    CODE_REGISTRY_ADDRESS,
    VOTE_ADDRESS,
    UPDATE_ADDRESS,
];

/// Whether a route may be credited native value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValuePolicy {
    /// No selector is payable.
    Reject,
    /// At least one selector is payable.
    Payable,
}

#[derive(Clone, Copy)]
pub(crate) struct ExactRoute {
    address: Address,
}

#[derive(Clone, Copy)]
pub(crate) enum Route {
    Exact(ExactRoute),
    StablecoinClass,
}

pub(crate) struct RouteCall<'input> {
    pub(crate) callee: Address,
    pub(crate) data: &'input [u8],
    pub(crate) caller: Address,
    pub(crate) value: U256,
}

impl Route {
    /// The value policy the boundary applies to this route.
    pub(crate) fn value_policy(self, host: &dyn crate::EvmRuntimeHost) -> ValuePolicy {
        match self {
            Self::Exact(route) => {
                if host.precompile_is_payable(&route.address) {
                    ValuePolicy::Payable
                } else {
                    ValuePolicy::Reject
                }
            }
            Self::StablecoinClass => ValuePolicy::Payable,
        }
    }

    pub(crate) fn base_gas(self, host: &dyn crate::EvmRuntimeHost, input: &[u8]) -> u64 {
        match self {
            Self::Exact(route) => host.precompile_base_gas(&route.address, input),
            Self::StablecoinClass => {
                outbe_primitives::stablecoin_fork::STABLECOIN_V1_GAS.dispatch_base
            }
        }
    }

    pub(crate) fn dispatch(
        self,
        host: &dyn crate::EvmRuntimeHost,
        storage: StorageHandle,
        call: RouteCall<'_>,
    ) -> Result<Bytes> {
        let address = match self {
            Self::Exact(route) => route.address,
            Self::StablecoinClass => call.callee,
        };
        host.dispatch_precompile(storage, &address, call.data, call.caller, call.value)
    }
}

#[cfg(test)]
impl ExactRoute {
    pub(crate) fn base_gas(self, host: &dyn crate::EvmRuntimeHost, input: &[u8]) -> u64 {
        host.precompile_base_gas(&self.address, input)
    }
}

pub(crate) fn lookup(address: &Address) -> Option<ExactRoute> {
    EXACT_ADDRESSES
        .contains(address)
        .then_some(ExactRoute { address: *address })
}

pub(crate) fn resolve(address: &Address) -> Option<Route> {
    lookup(address)
        .map(Route::Exact)
        .or_else(|| is_stablecoin_address(*address).then_some(Route::StablecoinClass))
}

const fn addresses_equal(left: Address, right: Address) -> bool {
    let mut index = 0;
    while index < 20 {
        if left.0 .0[index] != right.0 .0[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn is_ethereum_precompile(address: Address) -> bool {
    let mut index = 0;
    while index < 19 {
        if address.0 .0[index] != 0 {
            return false;
        }
        index += 1;
    }
    address.0 .0[19] >= 1 && address.0 .0[19] <= 10
}

const fn assert_valid_production_routes(addresses: &[Address]) {
    let mut index = 0;
    while index < addresses.len() {
        if is_ethereum_precompile(addresses[index]) {
            panic!("Outbe exact route overlaps an Ethereum precompile");
        }
        if is_stablecoin_address(addresses[index]) {
            panic!("Outbe exact route overlaps the Stablecoin address class");
        }
        let mut other = index + 1;
        while other < addresses.len() {
            if addresses_equal(addresses[index], addresses[other]) {
                panic!("duplicate Outbe exact route");
            }
            other += 1;
        }
        index += 1;
    }
}

const _: () = assert_valid_production_routes(EXACT_ADDRESSES);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationError {
    Duplicate(Address),
    EthereumOverlap(Address),
}

#[cfg(test)]
fn validate_exact_addresses(addresses: &[Address]) -> std::result::Result<(), ValidationError> {
    for (index, address) in addresses.iter().copied().enumerate() {
        if is_ethereum_precompile(address) {
            return Err(ValidationError::EthereumOverlap(address));
        }
        if addresses[index + 1..].contains(&address) {
            return Err(ValidationError::Duplicate(address));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_exact_routes_validate() {
        assert_eq!(validate_exact_addresses(EXACT_ADDRESSES), Ok(()));
    }

    #[test]
    fn duplicate_and_ethereum_overlap_are_rejected() {
        let duplicate = Address::repeat_byte(0x44);
        assert_eq!(
            validate_exact_addresses(&[duplicate, duplicate]),
            Err(ValidationError::Duplicate(duplicate))
        );

        let ethereum = Address::with_last_byte(1);
        assert_eq!(
            validate_exact_addresses(&[ethereum]),
            Err(ValidationError::EthereumOverlap(ethereum))
        );
    }

    #[test]
    fn code_registry_route_uses_standard_base_gas() {
        let route = lookup(&CODE_REGISTRY_ADDRESS).expect("CodeRegistry route");
        let host = crate::host::stub_host();
        assert_eq!(route.base_gas(host.as_ref(), &[]), PRECOMPILE_BASE_GAS);
        assert_eq!(
            route.base_gas(host.as_ref(), &[0xde, 0xad, 0xbe, 0xef]),
            PRECOMPILE_BASE_GAS
        );
    }

    #[test]
    fn stablecoin_class_address_resolves() {
        let mut bytes = [0u8; 20];
        bytes[..2].copy_from_slice(&STABLECOIN_ADDRESS_PREFIX);
        bytes[19] = 0x11;
        assert!(matches!(
            resolve(&Address::from(bytes)),
            Some(Route::StablecoinClass)
        ));
    }
}
