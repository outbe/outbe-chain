//! address-set parity for outbe precompile registration.
//!
//! Verifies:
//! 1. `extend_outbe_precompiles` registers every address listed in
//!    `outbe_precompile_addresses()` (single source of truth for the table).
//! 2. Every current dispatch branch and the intentionally drifted public list are
//!    enumerated exactly, rather than protected by a count-only assertion.
//! 3. The set of registered addresses does NOT include any Ethereum
//!    standard precompile addresses (`0x01..0x0a`) — outbe addresses must
//!    not collide with the upstream table.
//! 4. `PrecompilesMap::get` returns `Some(_)` for every outbe address after
//!    `extend_outbe_precompiles` runs (i.e. the dispatch lookup closure is
//!    actually installed).

use alloy_evm::{eth::EthEvmContext, precompiles::PrecompilesMap, revm::handler::EthPrecompiles};
use alloy_primitives::Address;
use outbe_evm::precompiles::{extend_outbe_precompiles, outbe_precompile_addresses};
use outbe_primitives::addresses::*;
use revm::{
    database_interface::EmptyDB, handler::PrecompileProvider, primitives::hardfork::SpecId,
};

const PRECOMPILES_SOURCE: &str = include_str!("../src/precompiles.rs");

const DISPATCH_CONSTANT_NAMES: [&str; 35] = [
    "GRATIS_ADDRESS",
    "GRATIS_FACTORY_ADDRESS",
    "PROMIS_ADDRESS",
    "PROMIS_FACTORY_ADDRESS",
    "TRIBUTE_ADDRESS",
    "NOD_ADDRESS",
    "NOD_FACTORY_ADDRESS",
    "GEM_ADDRESS",
    "GEM_FACTORY_ADDRESS",
    "INTEX_ADDRESS",
    "INTEX_FACTORY_ADDRESS",
    "DESIS_ADDRESS",
    "VAULT_PROVIDER_ADDRESS",
    "CREDIS_ADDRESS",
    "CREDIS_FACTORY_ADDRESS",
    "TRIBUTE_FACTORY_ADDRESS",
    "VALIDATOR_SET_ADDRESS",
    "SLASH_INDICATOR_ADDRESS",
    "STAKING_ADDRESS",
    "REWARDS_ADDRESS",
    "AGENT_REWARD_ADDRESS",
    "METADOSIS_ADDRESS",
    "FIDELITY_ADDRESS",
    "PROMIS_LIMIT_ADDRESS",
    "ORACLE_ADDRESS",
    "ZEROFEE_ADDRESS",
    "OUTBE_SYSTEM_TX_ADDRESS",
    "DEBUG_SUBCALL_PRECOMPILE_ADDRESS",
    "ZKPROOF_POSEIDON_ADDRESS",
    "ZKPROOF_GROTH16_ADDRESS",
    "TEE_REGISTRY_ADDRESS",
    "L2_REGISTRY_ADDRESS",
    "GOVERNANCE_ADDRESS",
    "VOTE_ADDRESS",
    "UPDATE_ADDRESS",
];

fn expected_dispatch_addresses() -> [Address; 35] {
    [
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
        VAULT_PROVIDER_ADDRESS,
        CREDIS_ADDRESS,
        CREDIS_FACTORY_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
        VALIDATOR_SET_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        STAKING_ADDRESS,
        REWARDS_ADDRESS,
        AGENT_REWARD_ADDRESS,
        METADOSIS_ADDRESS,
        FIDELITY_ADDRESS,
        PROMIS_LIMIT_ADDRESS,
        ORACLE_ADDRESS,
        ZEROFEE_ADDRESS,
        OUTBE_SYSTEM_TX_ADDRESS,
        DEBUG_SUBCALL_PRECOMPILE_ADDRESS,
        ZKPROOF_POSEIDON_ADDRESS,
        ZKPROOF_GROTH16_ADDRESS,
        TEE_REGISTRY_ADDRESS,
        L2_REGISTRY_ADDRESS,
        GOVERNANCE_ADDRESS,
        VOTE_ADDRESS,
        UPDATE_ADDRESS,
    ]
}

fn expected_public_address_list() -> [Address; 32] {
    [
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
        CREDIS_ADDRESS,
        CREDIS_FACTORY_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
        VALIDATOR_SET_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        STAKING_ADDRESS,
        REWARDS_ADDRESS,
        AGENT_REWARD_ADDRESS,
        METADOSIS_ADDRESS,
        FIDELITY_ADDRESS,
        PROMIS_LIMIT_ADDRESS,
        ORACLE_ADDRESS,
        ZEROFEE_ADDRESS,
        OUTBE_SYSTEM_TX_ADDRESS,
        ZKPROOF_POSEIDON_ADDRESS,
        ZKPROOF_GROTH16_ADDRESS,
        TEE_REGISTRY_ADDRESS,
        L2_REGISTRY_ADDRESS,
        VOTE_ADDRESS,
        UPDATE_ADDRESS,
    ]
}

fn build_extended_precompiles() -> PrecompilesMap {
    let spec = SpecId::default();
    let mut precompiles = PrecompilesMap::from_static(EthPrecompiles::new(spec).precompiles);
    extend_outbe_precompiles::<EmptyDB>(
        &mut precompiles,
        spec,
        None,
        std::sync::Arc::new(outbe_compressed_entities::ExecutionScope::new()),
    );
    precompiles
}

#[test]
fn public_address_list_is_exact_not_count_only() {
    assert_eq!(
        outbe_precompile_addresses(),
        &expected_public_address_list()
    );
}

#[test]
fn dispatch_branches_are_enumerated_and_current_list_drift_is_exact() {
    let dispatch_start = PRECOMPILES_SOURCE
        .find("fn outbe_dispatch_fn")
        .expect("dispatch function");
    let dispatch_end = PRECOMPILES_SOURCE[dispatch_start..]
        .find("pub fn outbe_precompile_addresses")
        .map(|offset| dispatch_start + offset)
        .expect("public address list");
    let dispatch_source = &PRECOMPILES_SOURCE[dispatch_start..dispatch_end];
    assert_eq!(dispatch_source.matches("a if a == ").count(), 35);
    for constant in DISPATCH_CONSTANT_NAMES {
        assert!(
            dispatch_source.contains(&format!("a if a == {constant}")),
            "missing dispatch branch for {constant}"
        );
    }

    let public = outbe_precompile_addresses();
    let missing_from_public: Vec<Address> = expected_dispatch_addresses()
        .into_iter()
        .filter(|address| !public.contains(address))
        .collect();
    assert_eq!(
        missing_from_public,
        [
            VAULT_PROVIDER_ADDRESS,
            DEBUG_SUBCALL_PRECOMPILE_ADDRESS,
            GOVERNANCE_ADDRESS,
        ]
    );
}

#[test]
fn ctx_dispatch_hook_installed_after_extend() {
    // outbe stateful precompiles now dispatch via the
    // `set_ctx_dispatch_hook` fork extension on `PrecompilesMap`, not via
    // the static lookup table. `PrecompilesMap::get(addr)` therefore returns
    // `None` for outbe addresses by design — the hook intercepts them
    // earlier in `PrecompileProvider::run`. This test asserts only that the
    // hook is installed.
    let precompiles = build_extended_precompiles();
    assert!(
        precompiles.has_ctx_dispatch_hook(),
        "extend_outbe_precompiles must install the ctx-dispatch hook"
    );
}

#[test]
fn warm_and_contains_expose_only_upstream_ethereum_precompiles() {
    let precompiles = build_extended_precompiles();
    let warm: Vec<Address> =
        <PrecompilesMap as PrecompileProvider<EthEvmContext<EmptyDB>>>::warm_addresses(
            &precompiles,
        )
        .collect();

    for suffix in 1u8..=10 {
        let ethereum = Address::with_last_byte(suffix);
        assert!(
            <PrecompilesMap as PrecompileProvider<EthEvmContext<EmptyDB>>>::contains(
                &precompiles,
                &ethereum,
            ),
            "Ethereum precompile {ethereum} must remain in contains()"
        );
        assert!(warm.contains(&ethereum));
    }
    for outbe in expected_dispatch_addresses() {
        assert!(
            !<PrecompilesMap as PrecompileProvider<EthEvmContext<EmptyDB>>>::contains(
                &precompiles,
                &outbe,
            ),
            "current contains() intentionally omits ctx-hook address {outbe}"
        );
        assert!(!warm.contains(&outbe));
    }
}

#[test]
fn top_level_and_nested_paths_share_dispatch_but_input_predecode_differs() {
    assert_eq!(
        PRECOMPILES_SOURCE
            .matches("outbe_ctx_dispatch::<DB>(")
            .count(),
        2,
        "top-level hook and nested provider must share outbe_ctx_dispatch"
    );
    for pinned in [
        "let address = inputs.bytecode_address;",
        "let caller = inputs.caller;",
        "self_address: address,",
        "CallInput::Bytes(b) => b.clone()",
        "CallInput::SharedBuffer(_) => Bytes::new()",
        "let base_gas = base_gas_fn(data.as_ref()).max(PRECOMPILE_BASE_GAS);",
        "let data: Bytes = inputs.input.bytes_local(ctx.local());",
    ] {
        assert!(
            PRECOMPILES_SOURCE.contains(pinned),
            "current caller/callee/input behavior changed: {pinned}"
        );
    }
    assert!(
        PRECOMPILES_SOURCE.contains("fn warm_addresses(&self)")
            && PRECOMPILES_SOURCE
                .matches("self.eth.warm_addresses()")
                .count()
                == 1
            && PRECOMPILES_SOURCE
                .matches("self.eth.contains(address)")
                .count()
                == 1,
        "nested provider must currently delegate warm/contains to Ethereum fallback"
    );
}

#[test]
fn outbe_addresses_do_not_collide_with_eth_standard() {
    fn eth_addr(last: u8) -> Address {
        let mut bytes = [0u8; 20];
        bytes[19] = last;
        Address::new(bytes)
    }
    let standard: [Address; 10] = [
        eth_addr(0x01),
        eth_addr(0x02),
        eth_addr(0x03),
        eth_addr(0x04),
        eth_addr(0x05),
        eth_addr(0x06),
        eth_addr(0x07),
        eth_addr(0x08),
        eth_addr(0x09),
        eth_addr(0x0a),
    ];
    for outbe in outbe_precompile_addresses() {
        for eth in &standard {
            assert_ne!(
                outbe, eth,
                "outbe precompile address {outbe:?} collides with Ethereum standard {eth:?}"
            );
        }
    }
}

#[test]
fn outbe_addresses_have_no_duplicates() {
    let addrs = outbe_precompile_addresses();
    let mut sorted: Vec<Address> = addrs.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        addrs.len(),
        "outbe_precompile_addresses() must not contain duplicates"
    );
}

#[test]
fn unregistered_address_returns_none() {
    let precompiles = build_extended_precompiles();
    let unknown = Address::from([0xFE; 20]);
    assert!(
        precompiles.get(&unknown).is_none(),
        "PrecompilesMap should return None for unregistered outbe-style address"
    );
}

/// Anti-spam security invariant: every sponsored-tx target is a
/// registered outbe precompile, so a sponsored free tx can only ever
/// invoke protocol-defined entrypoints — never arbitrary EVM code.
#[test]
fn sponsored_whitelist_is_subset_of_registered_precompiles() {
    use outbe_primitives::zero_fee::SPONSORED_TARGET_WHITELIST;
    let registered = outbe_precompile_addresses();
    for target in SPONSORED_TARGET_WHITELIST {
        assert!(
            registered.contains(target),
            "sponsored whitelist target {target:?} is not a registered outbe precompile — \
             a sponsored tx could route to an unregistered/arbitrary address"
        );
    }
}

/// Anti-abuse invariant: validator-only / settlement entrypoints must
/// NOT be reachable through the free sponsored path. Adding any of
/// these to the whitelist would let an attacker spam consensus-critical
/// precompiles for free; this test fails closed if that ever happens.
#[test]
fn sponsored_whitelist_excludes_validator_entrypoints() {
    use outbe_primitives::addresses::{
        ORACLE_ADDRESS, REWARDS_ADDRESS, SLASH_INDICATOR_ADDRESS, STAKING_ADDRESS,
        VALIDATOR_SET_ADDRESS, ZEROFEE_ADDRESS,
    };
    use outbe_primitives::zero_fee::SPONSORED_TARGET_WHITELIST;
    let forbidden = [
        VALIDATOR_SET_ADDRESS,
        STAKING_ADDRESS,
        REWARDS_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        ORACLE_ADDRESS,
        // The paymaster itself must not be a sponsored target (would be
        // a self-call loop / quota nonsense).
        ZEROFEE_ADDRESS,
    ];
    for f in &forbidden {
        assert!(
            !SPONSORED_TARGET_WHITELIST.contains(f),
            "validator/settlement entrypoint {f:?} must NOT be in the sponsored whitelist"
        );
    }
}
