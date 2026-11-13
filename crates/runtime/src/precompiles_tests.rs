//! The runtime route table and exact-address enumeration name the same precompiles.

use alloy_primitives::Address;
use outbe_evm::EvmRuntimeHost;
use outbe_system_common::addresses::{
    CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, CROSSCHAIN_BRIDGE_ADDRESS, INTEX_FACTORY_ADDRESS,
    STABLECOIN_ADDRESS_PREFIX, STAKING_ADDRESS, VOTE_ADDRESS,
};

use super::{exact_route, outbe_exact_precompile_addresses, ROUTE_COUNT};

const EXPECTED_PAYABLE_EXACT: [Address; 6] = [
    STAKING_ADDRESS,
    INTEX_FACTORY_ADDRESS,
    VOTE_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    CCA_REGISTRY_ADDRESS,
    CROSSCHAIN_BRIDGE_ADDRESS,
];

#[test]
fn exact_routes_match_enumeration_registration_and_value_policy() {
    let addresses = outbe_exact_precompile_addresses();
    assert_eq!(ROUTE_COUNT, addresses.len());
    let host = crate::OutbeRuntime::new();
    for address in addresses {
        assert!(
            exact_route(address).is_some(),
            "missing exact route for {address}"
        );
        assert!(host.is_registered_precompile(address));
        assert_eq!(
            host.precompile_is_payable(address),
            EXPECTED_PAYABLE_EXACT.contains(address),
            "unexpected value policy for {address:#x}"
        );
    }

    let mut stablecoin = Address::ZERO;
    stablecoin.0 .0[..STABLECOIN_ADDRESS_PREFIX.len()].copy_from_slice(&STABLECOIN_ADDRESS_PREFIX);
    stablecoin.0 .0[19] = 1;
    assert!(exact_route(&stablecoin).is_none());
    assert!(host.is_registered_precompile(&stablecoin));
    assert!(host.precompile_is_payable(&stablecoin));
}
