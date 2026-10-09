use super::*;

/// The other attestation mode.
fn other_mode(mode: AttestationMode) -> AttestationMode {
    match mode {
        AttestationMode::DcapRequired => AttestationMode::GramineDirectDev,
        AttestationMode::GramineDirectDev => AttestationMode::DcapRequired,
    }
}

/// The [`hardening_policy`] of `genesis_hash` in the attestation mode `mode`.
fn policy_in_mode(genesis_hash: B256, mode: AttestationMode) -> TeePolicyV1 {
    let mut policy = hardening_policy(genesis_hash);
    policy.attestation_mode = mode;
    policy
}

/// The [`successor_policy`] of `current` in the other attestation mode.
fn cross_mode_successor(current: &TeePolicyV1) -> TeePolicyV1 {
    let mut successor = successor_policy(current);
    successor.attestation_mode = other_mode(current.attestation_mode);
    successor
}

/// Asserts that the install of `initial` on `storage` reverts at the
/// attestation mode before a registry write.
fn assert_install_rejected_without_writes(storage: StorageHandle<'_>, initial: &TeePolicyV1) {
    let mut registry = TeeRegistry::new(storage);
    assert_reverts(
        registry.install_initial_policy_v1(initial),
        "attestation mode",
    );
    assert_eq!(registry.active_v1_policy_len.read().unwrap(), 0);
    assert!(registry.active_v1_policy_hash.read().unwrap().is_zero());
}

#[test]
fn policy_hash_admission_preserves_legacy_and_strict_upgrade_windows() {
    let active = B256::repeat_byte(0xa1);
    let successor = B256::repeat_byte(0xb2);
    let unrelated = B256::repeat_byte(0xc3);
    let mut provider = storage(B256::repeat_byte(0xd4));

    StorageHandle::enter(&mut provider, |storage| {
        let registry = TeeRegistry::new(storage);
        registry.active_v1_policy_hash.write(active).unwrap();
        registry.staged_v1_policy_hash.write(successor).unwrap();

        assert!(registry.policy_hash_admitted_v1(active, false).unwrap());
        assert!(registry.policy_hash_admitted_v1(successor, true).unwrap());
        assert!(!registry.policy_hash_admitted_v1(active, true).unwrap());
        assert!(!registry.policy_hash_admitted_v1(successor, false).unwrap());

        registry
            .strict_upgrade_proposal
            .write(U256::from(9))
            .unwrap();
        registry.strict_upgrade_height.write(50).unwrap();
        registry.strict_upgrade_successor.write(successor).unwrap();
        registry.strict_upgrade_predecessor.write(active).unwrap();

        assert!(registry.policy_hash_admitted_v1(active, false).unwrap());
        assert!(!registry.policy_hash_admitted_v1(active, true).unwrap());
        assert!(registry.policy_hash_admitted_v1(successor, false).unwrap());
        assert!(registry.policy_hash_admitted_v1(successor, true).unwrap());
        assert!(!registry.policy_hash_admitted_v1(unrelated, false).unwrap());
    });

    provider.set_block_number(50);
    StorageHandle::enter(&mut provider, |storage| {
        let registry = TeeRegistry::new(storage);
        assert!(!registry.policy_hash_admitted_v1(active, false).unwrap());
        assert!(!registry.policy_hash_admitted_v1(successor, true).unwrap());

        registry.active_v1_policy_hash.write(successor).unwrap();
        assert!(registry.policy_hash_admitted_v1(successor, false).unwrap());
        assert!(registry.policy_hash_admitted_v1(successor, true).unwrap());
        assert!(!registry.policy_hash_admitted_v1(active, false).unwrap());
    });
}

#[test]
fn initial_policy_is_state_authority_and_is_write_once() {
    let genesis_hash = B256::repeat_byte(0x14);
    let first = hardening_policy(genesis_hash);
    let mut provider = storage(genesis_hash);
    let bootstrap_policy_hash = B256::repeat_byte(0xD1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.policy_hash.write(bootstrap_policy_hash).unwrap();
        registry.install_initial_policy_v1(&first).unwrap();
        registry.install_initial_policy_v1(&first).unwrap();
        assert_eq!(registry.active_policy_v1().unwrap(), first);
        assert_eq!(registry.policy_hash.read().unwrap(), bootstrap_policy_hash);
        assert_eq!(
            registry.active_v1_policy_hash.read().unwrap(),
            first.policy_hash().unwrap()
        );

        let mut conflicting = first.clone();
        conflicting.minimum_tcb_evaluation_data_number = 2;
        assert_reverts(
            registry.install_initial_policy_v1(&conflicting),
            "already installed",
        );
    });

    let mut wrong_chain_provider = storage(genesis_hash);
    let mut wrong_chain = first;
    wrong_chain.chain_id = U256::from(2).to_be_bytes();
    StorageHandle::enter(&mut wrong_chain_provider, |storage| {
        assert_reverts(
            TeeRegistry::new(storage).install_initial_policy_v1(&wrong_chain),
            "chain identity mismatch",
        );
    });
}

#[test]
fn initial_policy_rejects_direct_dev_on_mainnet_before_registry_writes() {
    let genesis_hash = B256::repeat_byte(0x19);
    let mut direct = policy_in_mode(genesis_hash, AttestationMode::GramineDirectDev);
    direct.chain_id = U256::from(MAINNET_CHAIN_ID).to_be_bytes();
    let mut provider = storage_for_chain(MAINNET_CHAIN_ID, genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        assert_install_rejected_without_writes(storage, &direct);
    });
}

#[test]
fn initial_policy_accepts_both_non_mainnet_modes_and_mainnet_dcap() {
    for (chain_id, mode) in [
        (DEVNET_CHAIN_ID, AttestationMode::DcapRequired),
        (DEVNET_CHAIN_ID, AttestationMode::GramineDirectDev),
        (TESTNET_CHAIN_ID, AttestationMode::DcapRequired),
        (TESTNET_CHAIN_ID, AttestationMode::GramineDirectDev),
        (MAINNET_CHAIN_ID, AttestationMode::DcapRequired),
    ] {
        let genesis_hash = B256::from(U256::from(chain_id).to_be_bytes());
        let mut initial = policy_in_mode(genesis_hash, mode);
        initial.chain_id = U256::from(chain_id).to_be_bytes();
        let mut provider = storage_for_chain(chain_id, genesis_hash);
        StorageHandle::enter(&mut provider, |storage| {
            let registry = installed_registry(storage, &initial);
            assert_eq!(registry.active_policy_v1().unwrap(), initial);
        });
    }
}

#[test]
fn initial_policy_rejects_both_modes_on_an_unknown_chain_before_registry_writes() {
    const UNKNOWN_CHAIN_ID: u64 = TESTNET_CHAIN_ID + 1;

    for mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        let genesis_hash = B256::repeat_byte(mode as u8);
        let mut initial = policy_in_mode(genesis_hash, mode);
        initial.chain_id = U256::from(UNKNOWN_CHAIN_ID).to_be_bytes();
        let mut provider = storage_for_chain(UNKNOWN_CHAIN_ID, genesis_hash);

        StorageHandle::enter(&mut provider, |storage| {
            assert_install_rejected_without_writes(storage, &initial);
        });
    }
}

#[test]
fn stages_exactly_one_predecessor_bound_successor_policy() {
    let genesis_hash = B256::repeat_byte(0x15);
    let current = hardening_policy(genesis_hash);
    let mut successor = measurement_successor(&current, B256::repeat_byte(0x91));
    successor.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;

    run_installed(&current, |_storage, mut registry| {
        let mut wrong_genesis = successor.clone();
        wrong_genesis.genesis_hash = B256::repeat_byte(0xee);
        assert_reverts(
            registry.stage_successor_policy_v1(U256::from(5), &wrong_genesis),
            "chain identity",
        );

        let mut wrong_version = successor.clone();
        wrong_version.policy_version = 3;
        assert_reverts(
            registry.stage_successor_policy_v1(U256::from(6), &wrong_version),
            "current plus one",
        );

        let mut wrong_mode = successor.clone();
        wrong_mode.attestation_mode = AttestationMode::GramineDirectDev;
        assert_reverts(
            registry.stage_successor_policy_v1(U256::from(6), &wrong_mode),
            "attestation mode",
        );
        assert_eq!(registry.staged_successor_policy_v1().unwrap(), None);

        registry
            .stage_successor_policy_v1(U256::from(7), &successor)
            .unwrap();
        registry
            .stage_successor_policy_v1(U256::from(7), &successor)
            .unwrap();
        assert_eq!(
            registry.staged_successor_policy_v1().unwrap(),
            Some((U256::from(7), successor.clone()))
        );

        let mut conflicting = successor.clone();
        conflicting.minimum_tcb_evaluation_data_number = 2;
        assert_reverts(
            registry.stage_successor_policy_v1(U256::from(8), &conflicting),
            "already staged",
        );

        let mut wrong_predecessor = successor.clone();
        wrong_predecessor.predecessor_policy_hash = B256::repeat_byte(0xee);
        assert_reverts(
            TeeRegistry::new(registry.storage.clone())
                .stage_successor_policy_v1(U256::from(9), &wrong_predecessor),
            "predecessor",
        );
    });
}

#[test]
fn successor_policy_rejects_both_attestation_mode_switch_directions() {
    for active_mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        let genesis_hash = B256::repeat_byte(active_mode as u8);
        let current = policy_in_mode(genesis_hash, active_mode);
        let successor = cross_mode_successor(&current);
        run_installed(&current, |_storage, mut registry| {
            assert_reverts(
                registry.stage_successor_policy_v1(U256::from(20), &successor),
                "attestation mode",
            );
            assert_eq!(registry.staged_successor_policy_v1().unwrap(), None);
            assert_eq!(registry.active_policy_v1().unwrap(), current);
        });
    }
}

#[test]
fn promotes_staged_successor_exactly_at_activation_height_and_replays_idempotently() {
    for mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        let genesis_hash = B256::repeat_byte(mode as u8);
        let current = policy_in_mode(genesis_hash, mode);
        let mut successor = windowed_successor(&current);
        successor.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
        let proposal_id = U256::from(8);

        let mut provider = run_installed(&current, |_storage, mut registry| {
            registry
                .stage_successor_policy_v1(proposal_id, &successor)
                .unwrap();
            registry
                .stage_successor_policy_v1(proposal_id, &successor)
                .unwrap();
            assert_eq!(registry.active_policy_v1().unwrap(), current);
        });

        provider.set_block_number(50);
        StorageHandle::enter(&mut provider, |storage| {
            let mut registry = TeeRegistry::new(storage);
            registry
                .promote_staged_successor_policy_v1(proposal_id, 50)
                .unwrap();
            registry
                .promote_staged_successor_policy_v1(proposal_id, 50)
                .unwrap();
            assert_eq!(registry.active_policy_v1().unwrap(), successor);
            assert_eq!(registry.staged_successor_policy_v1().unwrap(), None);
        });
    }
}

/// Writes `successor` as the staged policy of `proposal_id` directly into the
/// staged-policy slots. This skips the checks of the staging call.
fn write_staged_policy_slots(
    registry: &TeeRegistry<'_>,
    proposal_id: U256,
    successor: &TeePolicyV1,
) {
    let canonical = successor.encode_canonical().unwrap();
    let policy_hash = successor.policy_hash().unwrap();
    for (index, chunk) in canonical.chunks(32).enumerate() {
        let mut word = [0u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        registry
            .staged_v1_policy_chunk
            .write(&(index as u32), B256::from(word))
            .unwrap();
    }
    registry
        .staged_v1_policy_len
        .write(canonical.len() as u32)
        .unwrap();
    registry.staged_v1_policy_hash.write(policy_hash).unwrap();
    registry
        .staged_v1_policy_proposal_id
        .write(proposal_id)
        .unwrap();
    registry
        .staged_v1_policy_activation_height
        .write(successor.activation_height)
        .unwrap();
}

#[test]
fn promotion_rejects_a_preexisting_cross_mode_successor() {
    for active_mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        let genesis_hash = B256::repeat_byte(active_mode as u8);
        let current = policy_in_mode(genesis_hash, active_mode);
        let successor = cross_mode_successor(&current);
        let proposal_id = U256::from(18);

        run_installed(&current, |_storage, mut registry| {
            write_staged_policy_slots(&registry, proposal_id, &successor);

            assert!(matches!(
                registry
                    .promote_staged_successor_policy_v1(proposal_id, 50)
                    .unwrap_err(),
                PrecompileError::Fatal(message) if message.contains("attestation mode")
            ));
            assert_eq!(registry.active_policy_v1().unwrap(), current);
            assert_eq!(
                registry.staged_successor_policy_v1().unwrap(),
                Some((proposal_id, successor))
            );
        });
    }
}

#[test]
fn ambiguous_measurement_rules_reject_at_the_registry_boundary() {
    let mut active_policy = hardening_policy(B256::repeat_byte(0x1a));
    let mut overlapping = active_policy.measurement_rules[0].clone();
    overlapping.minimum_isv_svn = 2;
    active_policy.measurement_rules.insert(0, overlapping);
    active_policy.encode_canonical().unwrap();

    let validator = LifecycleValidator::new(
        active_policy,
        0x3a,
        0x3b,
        EnclaveBindingSeeds::new(0x58, 0x68),
    );
    let signed_intent = validator.signed_initial();
    validator.run_as_validator(|_storage, mut registry| {
        assert_reverts(
            registry.register_enclave_after_verifier_for_test(
                signed_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            ),
            "exactly one",
        );
    });
}
