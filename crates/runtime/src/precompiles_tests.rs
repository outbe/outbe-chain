//! The runtime route table and the EVM address list name the same precompiles.

use alloy_primitives::Address;
use outbe_evm::EvmRuntimeHost;
use outbe_primitives::addresses::{
    CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, CROSSCHAIN_BRIDGE_ADDRESS, INTEX_FACTORY_ADDRESS,
    STABLECOIN_ADDRESS_PREFIX, STAKING_ADDRESS, VOTE_ADDRESS,
};

use super::{exact_route, is_payable, ROUTE_COUNT};

#[test]
fn exact_routes_match_the_evm_address_list_and_value_policy() {
    let addresses = outbe_evm::precompiles::outbe_precompile_addresses();
    assert_eq!(addresses.len(), 39);
    assert_eq!(ROUTE_COUNT, addresses.len());
    let host = crate::OutbeRuntime::new();
    for address in addresses {
        assert!(
            exact_route(address).is_some(),
            "missing exact route for {address}"
        );
        assert_eq!(
            is_payable(address),
            host.precompile_is_payable(address),
            "payable policy drifted for {address}"
        );
    }

    const PAYABLE: &[Address] = &[
        STAKING_ADDRESS,
        INTEX_FACTORY_ADDRESS,
        VOTE_ADDRESS,
        CREDIS_FACTORY_ADDRESS,
        CCA_REGISTRY_ADDRESS,
        CROSSCHAIN_BRIDGE_ADDRESS,
    ];
    for address in addresses {
        assert_eq!(
            is_payable(address),
            PAYABLE.contains(address),
            "unexpected value policy for {address}"
        );
    }

    let mut stablecoin = Address::ZERO;
    stablecoin.0 .0[..STABLECOIN_ADDRESS_PREFIX.len()].copy_from_slice(&STABLECOIN_ADDRESS_PREFIX);
    stablecoin.0 .0[19] = 1;
    assert!(exact_route(&stablecoin).is_none());
    assert_eq!(
        is_payable(&stablecoin),
        host.precompile_is_payable(&stablecoin)
    );
    assert!(is_payable(&stablecoin));
}
