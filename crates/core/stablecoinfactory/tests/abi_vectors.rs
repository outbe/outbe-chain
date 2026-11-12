use alloy_primitives::{hex, Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolError};

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoin.sol"
);
// Generated ABI methods follow the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IStablecoinFactory.sol"
    );
}
pub use abi::IStablecoinFactory;
sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoinPolicyRegistry.sol"
);
sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IVote.sol"
);

#[test]
fn alloy_encodes_canonical_dynamic_arguments() {
    let factory = IStablecoinFactory::predictTokenAddressCall {
        issuer: Address::with_last_byte(0x11),
        ticker: "EXUSD".to_owned(),
    }
    .abi_encode();
    assert_eq!(
        factory.as_slice(),
        &hex!("c5ab02800000000000000000000000000000000000000000000000000000000000000011000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000054558555344000000000000000000000000000000000000000000000000000000")
    );

    let directional = IStablecoinPolicyRegistry::createDirectionalPolicyCall {
        admin: Address::with_last_byte(0x22),
        sendPolicyId: U256::from(2),
        receivePolicyId: U256::from(3),
        mintPolicyId: U256::from(1),
    }
    .abi_encode();
    assert_eq!(
        directional.as_slice(),
        &hex!("baa1725c0000000000000000000000000000000000000000000000000000000000000022000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000030000000000000000000000000000000000000000000000000000000000000001")
    );

    let memo = IStablecoin::transferWithMemoCall {
        to: Address::with_last_byte(0x33),
        amount: U256::from(7),
        memo: B256::repeat_byte(0x44),
    }
    .abi_encode();
    assert_eq!(
        memo.as_slice(),
        &hex!("95777d59000000000000000000000000000000000000000000000000000000000000003300000000000000000000000000000000000000000000000000000000000000074444444444444444444444444444444444444444444444444444444444444444")
    );

    let bond_error = IVote::InvalidProposalBond {
        reason: "bond rejected".to_owned(),
    }
    .abi_encode();
    assert_eq!(
        bond_error.as_slice(),
        &hex!("1fa7abd90000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000d626f6e642072656a656374656400000000000000000000000000000000000000")
    );
}

#[test]
fn vote_status_filters_match_compact_abi_vectors() {
    for (status, expected) in [
        (
            IVote::ProposalStatus::Expired,
            hex!("42d1895700000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a"),
        ),
        (
            IVote::ProposalStatus::Error,
            hex!("42d1895700000000000000000000000000000000000000000000000000000000000000030000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a"),
        ),
    ] {
        let call = IVote::listProposalsByStatusCall {
            status,
            index: U256::ZERO,
            count: U256::from(10),
        };
        assert_eq!(call.abi_encode().as_slice(), &expected);
    }
}
