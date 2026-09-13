use super::*;

#[test]
fn initial_policy_is_state_authority_and_is_write_once() {
    let genesis_hash = B256::repeat_byte(0x14);
    let first = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
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
        assert!(revert_message(
            registry
                .install_initial_policy_v1(&conflicting)
                .unwrap_err()
        )
        .contains("already installed"));
    });

    let mut wrong_chain_provider = storage(genesis_hash);
    let mut wrong_chain = first;
    wrong_chain.chain_id = U256::from(2).to_be_bytes();
    StorageHandle::enter(&mut wrong_chain_provider, |storage| {
        assert!(revert_message(
            TeeRegistry::new(storage)
                .install_initial_policy_v1(&wrong_chain)
                .unwrap_err()
        )
        .contains("chain identity mismatch"));
    });
}

#[test]
fn initial_policy_rejects_direct_dev_on_mainnet_before_registry_writes() {
    let genesis_hash = B256::repeat_byte(0x19);
    let mut direct = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    direct.chain_id = U256::from(MAINNET_CHAIN_ID).to_be_bytes();
    direct.attestation_mode = AttestationMode::GramineDirectDev;
    let mut provider = storage_for_chain(MAINNET_CHAIN_ID, genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        assert!(
            revert_message(registry.install_initial_policy_v1(&direct).unwrap_err())
                .contains("attestation mode")
        );
        assert_eq!(registry.active_v1_policy_len.read().unwrap(), 0);
        assert!(registry.active_v1_policy_hash.read().unwrap().is_zero());
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
        let mut initial = policy(
            genesis_hash,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        );
        initial.chain_id = U256::from(chain_id).to_be_bytes();
        initial.attestation_mode = mode;
        let mut provider = storage_for_chain(chain_id, genesis_hash);
        StorageHandle::enter(&mut provider, |storage| {
            let mut registry = TeeRegistry::new(storage);
            registry.install_initial_policy_v1(&initial).unwrap();
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
        let mut initial = policy(
            genesis_hash,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        );
        initial.chain_id = U256::from(UNKNOWN_CHAIN_ID).to_be_bytes();
        initial.attestation_mode = mode;
        let mut provider = storage_for_chain(UNKNOWN_CHAIN_ID, genesis_hash);

        StorageHandle::enter(&mut provider, |storage| {
            let mut registry = TeeRegistry::new(storage);
            assert!(
                revert_message(registry.install_initial_policy_v1(&initial).unwrap_err())
                    .contains("attestation mode")
            );
            assert_eq!(registry.active_v1_policy_len.read().unwrap(), 0);
            assert!(registry.active_v1_policy_hash.read().unwrap().is_zero());
        });
    }
}

#[test]
fn stages_exactly_one_predecessor_bound_successor_policy() {
    let genesis_hash = B256::repeat_byte(0x15);
    let current = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut successor = current.clone();
    successor.policy_version = 2;
    successor.activation_height = 50;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    successor.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
    for rule in &mut successor.measurement_rules {
        rule.mrenclave = B256::repeat_byte(0x91);
        rule.admit_from_height = 50;
        rule.admit_until_height_exclusive = 500;
    }

    let mut provider = storage(genesis_hash);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&current).unwrap();

        let mut wrong_genesis = successor.clone();
        wrong_genesis.genesis_hash = B256::repeat_byte(0xee);
        assert!(revert_message(
            registry
                .stage_successor_policy_v1(U256::from(5), &wrong_genesis)
                .unwrap_err()
        )
        .contains("chain identity"));

        let mut wrong_version = successor.clone();
        wrong_version.policy_version = 3;
        assert!(revert_message(
            registry
                .stage_successor_policy_v1(U256::from(6), &wrong_version)
                .unwrap_err()
        )
        .contains("current plus one"));

        let mut wrong_mode = successor.clone();
        wrong_mode.attestation_mode = AttestationMode::GramineDirectDev;
        assert!(revert_message(
            registry
                .stage_successor_policy_v1(U256::from(6), &wrong_mode)
                .unwrap_err()
        )
        .contains("attestation mode"));
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
        assert!(revert_message(
            registry
                .stage_successor_policy_v1(U256::from(8), &conflicting)
                .unwrap_err()
        )
        .contains("already staged"));

        let mut wrong_predecessor = successor.clone();
        wrong_predecessor.predecessor_policy_hash = B256::repeat_byte(0xee);
        assert!(revert_message(
            TeeRegistry::new(registry.storage.clone())
                .stage_successor_policy_v1(U256::from(9), &wrong_predecessor)
                .unwrap_err()
        )
        .contains("predecessor"));
    });
}

#[test]
fn successor_policy_rejects_both_attestation_mode_switch_directions() {
    for active_mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        let genesis_hash = B256::repeat_byte(active_mode as u8);
        let mut current = policy(
            genesis_hash,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        );
        current.attestation_mode = active_mode;
        let mut successor = current.clone();
        successor.policy_version = 2;
        successor.activation_height = 50;
        successor.predecessor_policy_hash = current.policy_hash().unwrap();
        successor.attestation_mode = match active_mode {
            AttestationMode::DcapRequired => AttestationMode::GramineDirectDev,
            AttestationMode::GramineDirectDev => AttestationMode::DcapRequired,
        };
        let mut provider = storage(genesis_hash);
        StorageHandle::enter(&mut provider, |storage| {
            let mut registry = TeeRegistry::new(storage);
            registry.install_initial_policy_v1(&current).unwrap();
            assert!(revert_message(
                registry
                    .stage_successor_policy_v1(U256::from(20), &successor)
                    .unwrap_err()
            )
            .contains("attestation mode"));
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
        let mut current = policy(
            genesis_hash,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        );
        current.attestation_mode = mode;
        let mut successor = current.clone();
        successor.policy_version = 2;
        successor.activation_height = 50;
        successor.predecessor_policy_hash = current.policy_hash().unwrap();
        successor.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
        for rule in &mut successor.measurement_rules {
            rule.admit_from_height = 50;
            rule.admit_until_height_exclusive = 500;
        }
        let proposal_id = U256::from(8);
        let mut provider = storage(genesis_hash);

        StorageHandle::enter(&mut provider, |storage| {
            let mut registry = TeeRegistry::new(storage);
            registry.install_initial_policy_v1(&current).unwrap();
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

#[test]
fn promotion_rejects_a_preexisting_cross_mode_successor() {
    for active_mode in [
        AttestationMode::DcapRequired,
        AttestationMode::GramineDirectDev,
    ] {
        let genesis_hash = B256::repeat_byte(active_mode as u8);
        let mut current = policy(
            genesis_hash,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        );
        current.attestation_mode = active_mode;
        let mut successor = current.clone();
        successor.policy_version = 2;
        successor.activation_height = 50;
        successor.predecessor_policy_hash = current.policy_hash().unwrap();
        successor.attestation_mode = match active_mode {
            AttestationMode::DcapRequired => AttestationMode::GramineDirectDev,
            AttestationMode::GramineDirectDev => AttestationMode::DcapRequired,
        };
        let canonical = successor.encode_canonical().unwrap();
        let policy_hash = successor.policy_hash().unwrap();
        let proposal_id = U256::from(18);
        let mut provider = storage(genesis_hash);

        StorageHandle::enter(&mut provider, |storage| {
            let mut registry = TeeRegistry::new(storage);
            registry.install_initial_policy_v1(&current).unwrap();
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

            assert!(format!(
                "{}",
                registry
                    .promote_staged_successor_policy_v1(proposal_id, 50)
                    .unwrap_err()
            )
            .contains("attestation mode"));
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
    let genesis_hash = B256::repeat_byte(0x1a);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut overlapping = active_policy.measurement_rules[0].clone();
    overlapping.minimum_isv_svn = 2;
    active_policy.measurement_rules.insert(0, overlapping);
    active_policy.encode_canonical().unwrap();

    let node_signer = OutbeEvmSigner::from_secret_bytes([0x3a; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x3b; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x58,
        0x68,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("exactly one"));
    });
}
