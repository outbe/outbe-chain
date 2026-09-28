use super::super::*;
/// Every stateful dispatch-registered precompile must be preserved by either
/// the per-block EIP-161 marker list or canonical genesis marker bytecode;
/// otherwise state-root computation can silently discard its storage. This
/// unit pins runtime-marker coverage for routes that are neither stateless nor
/// genesis-preserved, while `tests/genesis.rs` binds the complementary seed.
#[test]
fn marker_list_covers_stateful_precompiles() {
    use crate::executor::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES;
    use crate::precompiles::outbe_precompile_addresses;
    use outbe_primitives::addresses::{
        GOVERNANCE_ADDRESS, RADICLE_REGISTRY_ADDRESS, STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS, VAULT_ROUTER_ADDRESS, ZEROFEE_ADDRESS,
        ZKPROOF_GROTH16_ADDRESS, ZKPROOF_POSEIDON_ADDRESS,
    };

    // Dispatch-registered precompiles that legitimately need NO runtime 0xEF
    // marker. Each state-owning exemption must have canonical genesis-marker
    // evidence in `tests/genesis.rs`; an unproven exemption permits silent pruning.
    const MARKER_EXEMPT: [Address; 8] = [
        // Stateless verifiers - no EVM storage to preserve.
        ZKPROOF_POSEIDON_ADDRESS,
        ZKPROOF_GROTH16_ADDRESS,
        // Seeded with genesis marker bytecode by scripts/seed_genesis.py, so these
        // accounts are never EIP-161-empty.
        ZEROFEE_ADDRESS,
        VAULT_ROUTER_ADDRESS,
        GOVERNANCE_ADDRESS,
        // Stablecoin Factory and Policy Registry marker code is genesis-active
        // even before Stablecoin V1 runtime activation.
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
        // RadicleRegistry is present from genesis even when no repositories
        // are configured because ALL_PRECOMPILE_ADDRESSES seeds its marker.
        RADICLE_REGISTRY_ADDRESS,
    ];

    for addr in outbe_precompile_addresses() {
        if MARKER_EXEMPT.contains(addr) {
            continue;
        }
        assert!(
            OUTBE_RUNTIME_MARKER_ADDRESSES.contains(addr),
            "stateful dispatch-registered precompile {addr} is missing from the EIP-161 \
             runtime marker list (OUTBE_RUNTIME_MARKER_ADDRESSES) - its storage would be \
             silently pruned at state-root. Add it to the marker list, or, if it \
             is stateless / genesis-seeded, to MARKER_EXEMPT with justification."
        );
    }

    // GEM and GEM_FACTORY must stay covered because both own persistent storage.
    use outbe_primitives::addresses::{GEM_ADDRESS, GEM_FACTORY_ADDRESS};
    assert!(OUTBE_RUNTIME_MARKER_ADDRESSES.contains(&GEM_ADDRESS));
    assert!(OUTBE_RUNTIME_MARKER_ADDRESSES.contains(&GEM_FACTORY_ADDRESS));
}
