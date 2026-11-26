//! Behavioral registration and enumeration contract for existing exact routes.

use alloy_primitives::{Address, B256};
use evm2::{
    evm::{precompile::PrecompileProvider as _, PrewarmSet},
    SpecId,
};
use outbe_evm::precompiles::{
    OutbePrecompileExecutionContext, OutbePrecompileProvider, OutbePrecompileRuntime,
};
use outbe_runtime::precompiles::outbe_exact_precompile_addresses;
use outbe_system_common::addresses::*;

fn expected_exact_addresses() -> [Address; 37] {
    [
        GRATIS_ADDRESS,
        GRATIS_FACTORY_ADDRESS,
        PROMIS_ADDRESS,
        PROMIS_FACTORY_ADDRESS,
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
        WORLDWIDE_DAY_ADDRESS,
        VALIDATOR_SET_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        STAKING_ADDRESS,
        REWARDS_ADDRESS,
        AGENT_REWARD_ADDRESS,
        EMISSION_ADDRESS,
        METADOSIS_ADDRESS,
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
    ]
}

fn build_extended_precompiles() -> OutbePrecompileProvider {
    let spec = SpecId::default();
    OutbePrecompileProvider::new(
        OutbePrecompileExecutionContext::new(spec, B256::ZERO),
        OutbePrecompileRuntime::new(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
}

#[test]
fn exact_enumeration_contains_every_characterized_route() {
    assert_eq!(
        outbe_exact_precompile_addresses(),
        &expected_exact_addresses()
    );
}

#[test]
fn provider_contains_ethereum_and_outbe_routes_while_warm_set_remains_ethereum_only() {
    let precompiles = build_extended_precompiles();
    let mut warm = PrewarmSet::default();
    precompiles.warm_addresses(&mut warm);
    let eth_addresses = precompiles.addresses();

    for suffix in 1u8..=10 {
        let ethereum = Address::with_last_byte(suffix);
        assert!(precompiles.contains(&ethereum));
        assert!(eth_addresses.contains(&ethereum));
        assert!(
            warm.is_warm(&ethereum),
            "Ethereum precompile {ethereum} must be in the warm set"
        );
    }
    for outbe in expected_exact_addresses() {
        assert!(
            precompiles.contains(&outbe),
            "provider must contain registered Outbe route {outbe}"
        );
        assert!(
            !eth_addresses.contains(&outbe),
            "Ethereum precompile enumeration must not include Outbe route {outbe}"
        );
        assert!(
            !warm.is_warm(&outbe),
            "warm set must remain Ethereum-only and exclude Outbe route {outbe}"
        );
    }
}

#[test]
fn exact_routes_do_not_collide_with_ethereum_precompiles() {
    for outbe in outbe_exact_precompile_addresses() {
        for suffix in 1u8..=10 {
            assert_ne!(*outbe, Address::with_last_byte(suffix));
        }
    }
}

#[test]
fn exact_routes_have_no_duplicates() {
    let addresses = outbe_exact_precompile_addresses();
    let mut sorted = addresses.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), addresses.len());
}

#[test]
fn unregistered_address_is_not_contained() {
    let precompiles = build_extended_precompiles();
    assert!(!precompiles.contains(&Address::repeat_byte(0xfe)));
}

/// Every sponsored target must remain an exact Outbe route. Otherwise a free
/// transaction could invoke arbitrary EVM code.
#[test]
fn sponsored_whitelist_is_subset_of_exact_routes() {
    use outbe_system_common::zero_fee::SPONSORED_TARGET_WHITELIST;

    for target in SPONSORED_TARGET_WHITELIST {
        assert!(
            outbe_exact_precompile_addresses().contains(target),
            "sponsored target {target:?} is not an exact Outbe route"
        );
    }
}

#[test]
fn sponsored_whitelist_includes_gemfactory_cashout_entrypoint() {
    use outbe_system_common::zero_fee::SPONSORED_TARGET_WHITELIST;

    assert!(
        SPONSORED_TARGET_WHITELIST.contains(&GEM_FACTORY_ADDRESS),
        "zero-balance Gem owners need sponsored settleGem and minePromis"
    );
}

#[test]
fn sponsored_whitelist_excludes_validator_entrypoints() {
    use outbe_system_common::zero_fee::SPONSORED_TARGET_WHITELIST;

    for forbidden in [
        VALIDATOR_SET_ADDRESS,
        STAKING_ADDRESS,
        REWARDS_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        ORACLE_ADDRESS,
        ZEROFEE_ADDRESS,
    ] {
        assert!(
            !SPONSORED_TARGET_WHITELIST.contains(&forbidden),
            "validator/settlement entrypoint {forbidden:?} must not be sponsored"
        );
    }
}

#[test]
fn removed_hash_address_is_unregistered() {
    let address = alloy_primitives::address!("000000000000000000000000000000000000ee07");
    assert!(!outbe_exact_precompile_addresses().contains(&address));
    assert!(!build_extended_precompiles().contains(&address));
}
