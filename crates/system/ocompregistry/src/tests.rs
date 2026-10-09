use alloy_primitives::{B256, U256};
use alloy_sol_types::SolCall;
use outbe_ocomp_protocol::{profile::ProtocolBundleV1, test_utils::minimal_capacity_profile};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;

use crate::{
    poc_schema_limits,
    precompile::{dispatch, IOcompRegistry},
    OcompProtocolAuthorityV1, OcompRegistry, OcompRequestProfile,
};

const CHAIN_ID: u64 = 42;
const ACTIVATION_HEIGHT: u64 = 32;

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn bundle() -> ProtocolBundleV1 {
    ProtocolBundleV1 {
        fork_id: hash(21),
        release_gate_authority_envelope_hash: hash(22),
        release_approval_policy_hash: hash(24),
        release_validator_command_artifact_hash: hash(25),
        migration_manifest_hash: hash(26),
        required_upgrade_handler_set_hash: hash(27),
        ..outbe_ocomp_protocol::test_utils::minimal_protocol_bundle()
    }
}

fn authority(genesis_hash: B256) -> OcompProtocolAuthorityV1 {
    let protocol_bundle = bundle();
    let limits = poc_schema_limits();
    let bundle_hash = protocol_bundle.protocol_bundle_hash(&limits).unwrap();
    OcompProtocolAuthorityV1 {
        request_profile: OcompRequestProfile {
            chain_id: CHAIN_ID,
            genesis_hash,
            fork_id: protocol_bundle.fork_id,
            protocol_bundle_hash: bundle_hash,
            correctness_profile_id: protocol_bundle.correctness_profile_id,
            capacity_profile: minimal_capacity_profile(),
            source_availability_policy_id: hash(44),
        },
        protocol_bundle,
    }
}

fn successor(genesis_hash: B256, activation_height: u64) -> crate::OcompSuccessorV1 {
    let predecessor = authority(genesis_hash);
    let mut protocol_bundle = predecessor.protocol_bundle.clone();
    protocol_bundle.protocol_version += 1;
    protocol_bundle.fork_id = hash(61);
    protocol_bundle.request_semantics_version += 1;
    protocol_bundle.lysis_program_semantics_hash = hash(62);
    let limits = poc_schema_limits();
    let protocol_bundle_hash = protocol_bundle.protocol_bundle_hash(&limits).unwrap();
    crate::OcompSuccessorV1 {
        activation_height,
        predecessor_protocol_bundle_hash: predecessor.request_profile.protocol_bundle_hash,
        authority: OcompProtocolAuthorityV1 {
            request_profile: OcompRequestProfile {
                fork_id: protocol_bundle.fork_id,
                protocol_bundle_hash,
                correctness_profile_id: protocol_bundle.correctness_profile_id,
                ..predecessor.request_profile
            },
            protocol_bundle,
        },
    }
}

#[test]
fn successor_errors_preserve_priority_at_the_protocol_version_limit() {
    let limits = poc_schema_limits();
    let mut predecessor = authority(hash(42));
    predecessor.protocol_bundle.protocol_version = u16::MAX;
    predecessor.request_profile.protocol_bundle_hash = predecessor
        .protocol_bundle
        .protocol_bundle_hash(&limits)
        .unwrap();
    let mut next = successor(hash(42), 100);
    next.predecessor_protocol_bundle_hash = predecessor.request_profile.protocol_bundle_hash;

    assert!(matches!(
        next.validate_against(&predecessor, 50, &limits),
        Err(PrecompileError::Fatal(message)) if message == "OCOMP protocol version overflow"
    ));

    next.activation_height = 50;
    assert!(matches!(
        next.validate_against(&predecessor, 50, &limits),
        Err(PrecompileError::Fatal(message))
            if message == "OCOMP successor violates predecessor or immutable-policy invariants"
    ));

    next.activation_height = 100;
    next.authority.request_profile.source_availability_policy_id = hash(45);
    assert!(matches!(
        next.validate_against(&predecessor, 50, &limits),
        Err(PrecompileError::Fatal(message))
            if message == "OCOMP successor violates predecessor or immutable-policy invariants"
    ));
}

#[test]
fn request_profile_reports_reserved_identity_before_capacity_violation() {
    let mut profile = authority(hash(42)).request_profile;
    profile.chain_id = 0;
    profile.capacity_profile.max_workers_per_domain = 5;

    assert!(matches!(
        profile.validate(),
        Err(PrecompileError::Fatal(message))
            if message == "OCOMP request profile contains a reserved zero identity"
    ));
}

#[test]
fn request_profile_reports_frozen_capacity_bounds() {
    let mut profile = authority(hash(42)).request_profile;
    profile.capacity_profile.max_workers_per_domain = 5;

    assert!(matches!(
        profile.validate(),
        Err(PrecompileError::Fatal(message))
            if message == "OCOMP request profile violates frozen PoC bounds"
    ));

    profile.capacity_profile.max_workers_per_domain = 4;
    profile.capacity_profile.max_reference_currencies = 0;
    assert!(matches!(
        profile.validate(),
        Err(PrecompileError::Fatal(message))
            if message == "OCOMP request profile violates frozen PoC bounds"
    ));
}

#[test]
fn fresh_genesis_install_is_visible_and_exact_replay_is_a_noop() {
    let genesis_hash = hash(42);
    let expected = authority(genesis_hash);
    let install_hash = hash(99);
    let limits = poc_schema_limits();
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
    provider.set_block_number(ACTIVATION_HEIGHT);

    provider
        .enter(|storage| {
            let mut registry = OcompRegistry::new(storage);
            registry.initialize_genesis_authority(
                &expected,
                install_hash,
                ACTIVATION_HEIGHT,
                ACTIVATION_HEIGHT,
                &limits,
            )?;
            assert_eq!(registry.active_authority(&limits)?, Some(expected.clone()));
            assert_eq!(registry.install_hash.read()?, install_hash);
            assert_eq!(registry.activation_height.read()?, ACTIVATION_HEIGHT);
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .unwrap();

    provider
        .enter(|storage| {
            OcompRegistry::new(storage).initialize_genesis_authority(
                &expected,
                install_hash,
                ACTIVATION_HEIGHT,
                ACTIVATION_HEIGHT,
                &limits,
            )
        })
        .unwrap();

    assert_eq!(
        provider
            .get_events(outbe_primitives::addresses::OCOMP_REGISTRY_ADDRESS)
            .len(),
        1
    );
}

#[test]
fn successor_activation_keeps_pinned_predecessor_until_retention_expires() {
    let genesis_hash = hash(42);
    let initial = authority(genesis_hash);
    let activation_height = 100;
    let next = successor(genesis_hash, activation_height);
    let proposal_id = U256::from(7);
    let old_lineage = hash(71);
    let new_lineage = hash(72);
    let retry_lineage = hash(73);
    let limits = poc_schema_limits();
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
    provider.set_block_number(ACTIVATION_HEIGHT);
    provider
        .enter(|storage| {
            OcompRegistry::new(storage).initialize_genesis_authority(
                &initial,
                hash(99),
                ACTIVATION_HEIGHT,
                ACTIVATION_HEIGHT,
                &limits,
            )
        })
        .unwrap();

    provider.set_block_number(50);
    provider
        .enter(|storage| {
            let mut registry = OcompRegistry::new(storage);
            assert_eq!(
                registry.pin_lineage(old_lineage, &limits)?,
                initial.request_profile.protocol_bundle_hash
            );
            registry.stage_successor(proposal_id, &next, &limits)
        })
        .unwrap();

    provider.set_block_number(activation_height);
    provider
        .enter(|storage| {
            let mut registry = OcompRegistry::new(storage);
            registry.promote_staged_successor(proposal_id, activation_height, &limits)?;
            assert_eq!(
                registry.pin_lineage(new_lineage, &limits)?,
                next.authority.request_profile.protocol_bundle_hash
            );
            assert_eq!(
                registry.pin_inherited_lineage(retry_lineage, old_lineage, &limits)?,
                initial.request_profile.protocol_bundle_hash
            );
            assert_eq!(
                registry.resolve_lineage(old_lineage)?,
                Some(initial.request_profile.protocol_bundle_hash)
            );
            assert_eq!(
                registry.authority_by_bundle_hash(
                    initial.request_profile.protocol_bundle_hash,
                    &limits,
                )?,
                Some(initial.clone())
            );
            assert!(!registry.try_retire_predecessor(activation_height, &limits)?);
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .unwrap();

    provider.set_block_number(activation_height + 1);
    let retire_at = provider
        .enter(|storage| {
            let mut registry = OcompRegistry::new(storage);
            registry.release_lineage(old_lineage, activation_height + 1, &limits)?;
            assert_eq!(
                registry
                    .retention_until
                    .read(&initial.request_profile.protocol_bundle_hash)?,
                0
            );
            registry.release_lineage(retry_lineage, activation_height + 1, &limits)?;
            registry
                .retention_until
                .read(&initial.request_profile.protocol_bundle_hash)
        })
        .unwrap();
    assert!(retire_at > activation_height + 1);

    provider.set_block_number(retire_at);
    provider
        .enter(|storage| {
            let mut registry = OcompRegistry::new(storage);
            assert!(registry.try_retire_predecessor(retire_at, &limits)?);
            assert_eq!(
                registry.authority_by_bundle_hash(
                    initial.request_profile.protocol_bundle_hash,
                    &limits,
                )?,
                None
            );
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .unwrap();
}

#[test]
fn precompile_exposes_active_staged_retiring_and_lineage_state() {
    let genesis_hash = hash(42);
    let initial = authority(genesis_hash);
    let activation_height = 100;
    let next = successor(genesis_hash, activation_height);
    let proposal_id = U256::from(8);
    let lineage = hash(81);
    let limits = poc_schema_limits();
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
    provider.set_block_number(ACTIVATION_HEIGHT);
    provider
        .enter(|storage| {
            let mut registry = OcompRegistry::new(storage.clone());
            registry.initialize_genesis_authority(
                &initial,
                hash(99),
                ACTIVATION_HEIGHT,
                ACTIVATION_HEIGHT,
                &limits,
            )?;
            registry.pin_lineage(lineage, &limits)?;
            registry.stage_successor(proposal_id, &next, &limits)?;

            let staged = dispatch(
                storage.clone(),
                &IOcompRegistry::stagedSuccessorCall {}.abi_encode(),
                alloy_primitives::Address::ZERO,
                U256::ZERO,
            )?;
            let staged = IOcompRegistry::stagedSuccessorCall::abi_decode_returns(&staged)
                .map_err(|error| crate::errors::corruption(error.to_string()))?;
            assert_eq!(staged.proposalId, proposal_id);
            assert_eq!(
                staged.canonicalSuccessor.as_ref(),
                next.encode_canonical(&limits)?.as_slice()
            );

            let lineage_hash = dispatch(
                storage,
                &IOcompRegistry::lineageProtocolBundleHashCall { lineage }.abi_encode(),
                alloy_primitives::Address::ZERO,
                U256::ZERO,
            )?;
            assert_eq!(
                IOcompRegistry::lineageProtocolBundleHashCall::abi_decode_returns(&lineage_hash)
                    .map_err(|error| crate::errors::corruption(error.to_string()))?,
                initial.request_profile.protocol_bundle_hash
            );
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .unwrap();

    provider.set_block_number(activation_height);
    provider
        .enter(|storage| {
            OcompRegistry::new(storage.clone()).promote_staged_successor(
                proposal_id,
                activation_height,
                &limits,
            )?;
            let retiring = dispatch(
                storage,
                &IOcompRegistry::retiringProtocolBundleHashCall {}.abi_encode(),
                alloy_primitives::Address::ZERO,
                U256::ZERO,
            )?;
            assert_eq!(
                IOcompRegistry::retiringProtocolBundleHashCall::abi_decode_returns(&retiring)
                    .map_err(|error| crate::errors::corruption(error.to_string()))?,
                initial.request_profile.protocol_bundle_hash
            );
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .unwrap();
}

#[test]
fn conflicting_genesis_replay_and_unavailable_inherited_lineage_fail_closed() {
    let genesis_hash = hash(42);
    let initial = authority(genesis_hash);
    let limits = poc_schema_limits();
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
    provider.set_block_number(ACTIVATION_HEIGHT);
    provider
        .enter(|storage| {
            let mut registry = OcompRegistry::new(storage);
            registry.initialize_genesis_authority(
                &initial,
                hash(99),
                ACTIVATION_HEIGHT,
                ACTIVATION_HEIGHT,
                &limits,
            )?;
            let mut changed = initial.clone();
            changed.request_profile.source_availability_policy_id = hash(45);
            assert!(registry
                .initialize_genesis_authority(
                    &changed,
                    hash(99),
                    ACTIVATION_HEIGHT,
                    ACTIVATION_HEIGHT,
                    &limits,
                )
                .is_err());
            assert!(registry
                .pin_inherited_lineage(hash(91), hash(92), &limits)
                .is_err());
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .unwrap();
}
