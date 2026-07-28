use alloy_primitives::{b256, hex, keccak256, Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolError, SolEvent};

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoin.sol"
);
sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoinFactory.sol"
);
sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStablecoinPolicyRegistry.sol"
);
sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IVote.sol"
);

#[test]
fn complete_exported_abis_match_golden_hashes() {
    let token = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/precompiles/abi-export/IStablecoin.json"
    ));
    let factory = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/precompiles/abi-export/IStablecoinFactory.json"
    ));
    let policy = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/precompiles/abi-export/IStablecoinPolicyRegistry.json"
    ));
    let vote = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/precompiles/abi-export/IVote.json"
    ));

    assert_eq!(
        canonical_abi_hash(token),
        b256!("1a8515c0408559770e2ee4932e83e994dddd49545873ed38be2b5721d3d4f5b9")
    );
    assert_eq!(
        canonical_abi_hash(factory),
        b256!("a6a5298b3e98f2adaa5b7137e85cf9c6a27cf4c5f50c84a3c80e2cf8846afa57")
    );
    assert_eq!(
        canonical_abi_hash(policy),
        b256!("173497d729677043bff9e1a0c2e25794d87435368210a51602409f17ad707741")
    );
    assert_eq!(
        canonical_abi_hash(vote),
        b256!("915426835b210f2ba818816a665f6452ff8193c9e7bf8819adcbfbe9718ab8f3")
    );
}

#[test]
fn alloy_call_selectors_match_solidity_vectors() {
    assert_eq!(
        IStablecoin::transferWithMemoCall::SELECTOR,
        [0x95, 0x77, 0x7d, 0x59]
    );
    assert_eq!(
        IStablecoin::forcedTransferWithMemoCall::SELECTOR,
        [0xea, 0x24, 0x5f, 0xb8]
    );
    assert_eq!(
        IStablecoinFactory::predictTokenAddressCall::SELECTOR,
        [0xc5, 0xab, 0x02, 0x80]
    );
    assert_eq!(
        IStablecoinPolicyRegistry::createDirectionalPolicyCall::SELECTOR,
        [0xba, 0xa1, 0x72, 0x5c]
    );
    assert_eq!(
        IVote::getProposalBondCall::SELECTOR,
        [0xb6, 0xe1, 0x93, 0x28]
    );
}

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
}

#[test]
fn alloy_event_and_error_selectors_match_solidity_vectors() {
    assert_eq!(
        IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH,
        b256!("3bc7cf7e0d900433f10a105db3704bd383cbe686a86099bff51166e3c9e5fe02")
    );
    assert_eq!(
        IStablecoin::TransferWithMemo::SIGNATURE_HASH,
        b256!("57bc7354aa85aed339e000bccffabbc529466af35f0772c8f8ee1145927de7f0")
    );
    assert_eq!(
        IVote::ProposalBondBurned::SIGNATURE_HASH,
        b256!("b7e5d2683c51bcb2296756a4963c8b5130542b95b0f975446e6f3ac3f02dfb89")
    );
    assert_eq!(
        IStablecoinPolicyRegistry::MembershipUnchanged::SELECTOR,
        [0x0c, 0xdf, 0x47, 0x3a]
    );
}

fn canonical_abi_hash(mut bytes: &[u8]) -> B256 {
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    keccak256(bytes)
}
