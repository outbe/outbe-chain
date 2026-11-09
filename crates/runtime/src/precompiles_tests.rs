//! The runtime route table and the EVM address list name the same precompiles.

use alloy_primitives::Address;
use outbe_evm::EvmRuntimeHost;
use outbe_primitives::addresses::STABLECOIN_ADDRESS_PREFIX;

use super::{exact_route, is_payable, EXACT_ADDRESSES};

#[test]
fn exact_routes_match_the_evm_address_list_and_value_policy() {
    assert_eq!(EXACT_ADDRESSES.len(), 39);
    let host = outbe_evm::test_support::DomainEvmHost::new();
    assert_eq!(
        EXACT_ADDRESSES,
        outbe_evm::precompiles::outbe_precompile_addresses()
    );
    for address in EXACT_ADDRESSES {
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
