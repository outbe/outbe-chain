use super::super::value_policy::{
    classify_boundary_value, BoundaryValue::Credited, BoundaryValue::Rejected,
};
use crate::precompile_routes::{self, ValuePolicy};
use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::{
    CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, DESIS_ADDRESS, GRATIS_ADDRESS,
    HYPERLANE_CONTROLLER_ADDRESS, INTEX_FACTORY_ADDRESS, STAKING_ADDRESS, VOTE_ADDRESS,
};
use revm::interpreter::CallValue;

const PAYABLE: [Address; 6] = [
    STAKING_ADDRESS,
    INTEX_FACTORY_ADDRESS,
    VOTE_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    CCA_REGISTRY_ADDRESS,
    // fund refills the float that pays Interchain Account dispatch fees.
    HYPERLANE_CONTROLLER_ADDRESS,
];

fn policy(address: Address) -> ValuePolicy {
    precompile_routes::resolve(&address)
        .expect("address must be a routed precompile")
        .value_policy()
}

/// Apparent value names an amount that was never transferred. Delegated
/// frames are refused before this point, so the arm is defensive.
#[test]
fn apparent_value_is_never_credited() {
    for address in PAYABLE {
        assert_eq!(
            classify_boundary_value(policy(address), &CallValue::Apparent(U256::from(7u64))),
            Rejected("outbe precompile: apparent value is not a transfer"),
        );
    }
}

#[test]
fn zero_value_dispatches_whatever_the_policy() {
    for address in [GRATIS_ADDRESS, DESIS_ADDRESS, STAKING_ADDRESS] {
        for value in [
            CallValue::Transfer(U256::ZERO),
            CallValue::Apparent(U256::ZERO),
        ] {
            assert_eq!(
                classify_boundary_value(policy(address), &value),
                Credited(U256::ZERO),
            );
        }
    }
}

/// `staking.stake`, `intexfactory.distribute` and `vote.createProposal` are
/// the payable selectors, so their addresses must still receive funded calls.
#[test]
fn transferred_value_is_credited_to_payable_routes() {
    let amount = U256::from(5u64);
    for address in PAYABLE {
        assert_eq!(
            classify_boundary_value(policy(address), &CallValue::Transfer(amount)),
            Credited(amount),
        );
    }
}

/// Desis dropped its payable `clearAuction`, so value sent there now has no
/// accounting path and must not strand at the address.
#[test]
fn transferred_value_to_a_reject_route_is_refused() {
    let amount = U256::from(5u64);
    for address in [GRATIS_ADDRESS, DESIS_ADDRESS] {
        assert_eq!(
            classify_boundary_value(policy(address), &CallValue::Transfer(amount)),
            Rejected("outbe precompile: non-payable address called with value"),
        );
    }
}

/// Reserving the stablecoin address class must not make native value
/// unspendable there. The class dispatch decides which addresses may keep it.
#[test]
fn the_stablecoin_class_permits_value() {
    let token: Address = "0x53c0000000000000000000000000000000000001"
        .parse()
        .expect("valid stablecoin class address");
    let amount = U256::from(5u64);
    assert_eq!(policy(token), ValuePolicy::Payable);
    assert_eq!(
        classify_boundary_value(policy(token), &CallValue::Transfer(amount)),
        Credited(amount),
    );
}

/// Pins which exact routes declare `Payable`. This catches an edit to the
/// route table. A module that grows a payable selector without publishing it
/// has that selector's funded calls refused. The route refuses them before dispatch,
/// and the module refuses them again. Thus the omission shows up as its own broken
/// entrypoint rather than as stranded value.
#[test]
fn only_the_expected_routes_accept_value_among_exact_routes() {
    for address in precompile_routes::EXACT_ADDRESSES {
        assert_eq!(
            policy(*address) == ValuePolicy::Payable,
            PAYABLE.contains(address),
            "unexpected value policy for {address:#x}"
        );
    }
}
