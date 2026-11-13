//! Host-delegated route resolution and compile-time route validation for Outbe precompiles.
//!
//! All concrete precompile addresses, gas schedules, and dispatch handlers are owned
//! by the runtime host implementation outside `outbe-evm`.

use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::{
    addresses::is_reserved_dynamic_precompile_address, error::Result, storage::StorageHandle,
};

/// Whether a route may be credited native value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValuePolicy {
    /// No selector is payable.
    Reject,
    /// At least one selector is payable.
    Payable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    address: Address,
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
        if host.precompile_is_payable(&self.address) {
            ValuePolicy::Payable
        } else {
            ValuePolicy::Reject
        }
    }

    pub(crate) fn base_gas(self, host: &dyn crate::EvmRuntimeHost, input: &[u8]) -> u64 {
        host.precompile_base_gas(&self.address, input)
    }

    pub(crate) fn dispatch(
        self,
        host: &dyn crate::EvmRuntimeHost,
        storage: StorageHandle,
        call: RouteCall<'_>,
    ) -> Result<Bytes> {
        debug_assert_eq!(self.address, call.callee);
        host.dispatch_precompile(storage, &self.address, call.data, call.caller, call.value)
    }
}

pub(crate) fn resolve(host: &dyn crate::EvmRuntimeHost, address: &Address) -> Option<Route> {
    host.is_registered_precompile(address)
        .then_some(Route { address: *address })
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

pub const fn assert_valid_production_routes(addresses: &[Address]) {
    let mut index = 0;
    while index < addresses.len() {
        if is_ethereum_precompile(addresses[index]) {
            panic!("Outbe exact route overlaps an Ethereum precompile");
        }
        if is_reserved_dynamic_precompile_address(addresses[index]) {
            panic!("Outbe exact route overlaps the reserved dynamic precompile class");
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

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::{
        addresses::{CODE_REGISTRY_ADDRESS, RESERVED_DYNAMIC_PRECOMPILE_PREFIX},
        storage::gas::PRECOMPILE_BASE_GAS,
    };

    #[test]
    #[should_panic(expected = "duplicate Outbe exact route")]
    fn duplicate_exact_route_is_rejected() {
        let duplicate = Address::repeat_byte(0x44);
        assert_valid_production_routes(&[duplicate, duplicate]);
    }

    #[test]
    #[should_panic(expected = "Outbe exact route overlaps an Ethereum precompile")]
    fn ethereum_precompile_overlap_is_rejected() {
        assert_valid_production_routes(&[Address::with_last_byte(1)]);
    }

    #[test]
    #[should_panic(expected = "Outbe exact route overlaps the reserved dynamic precompile class")]
    fn reserved_dynamic_prefix_overlap_is_rejected() {
        let mut dynamic = [0u8; 20];
        dynamic[..2].copy_from_slice(&RESERVED_DYNAMIC_PRECOMPILE_PREFIX);
        dynamic[19] = 0x11;
        assert_valid_production_routes(&[Address::from(dynamic)]);
    }

    #[test]
    fn stub_host_is_fail_closed_and_charges_standard_base_gas() {
        let host = crate::host::stub_host();
        assert_eq!(resolve(host.as_ref(), &CODE_REGISTRY_ADDRESS), None);
        let route = Route {
            address: CODE_REGISTRY_ADDRESS,
        };
        assert_eq!(route.base_gas(host.as_ref(), &[]), PRECOMPILE_BASE_GAS);
        assert_eq!(
            route.base_gas(host.as_ref(), &[0xde, 0xad, 0xbe, 0xef]),
            PRECOMPILE_BASE_GAS
        );
    }
}
