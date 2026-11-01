use super::fixtures::assert_revert_bytes;
use super::*;
use alloy_sol_types::{SolError, SolEvent};
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;

const BLOCK_NUMBER: u64 = 10;
const ACTIVATION_HEIGHT: u64 = 50;

fn fresh_rejoin_intent(
    current: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = replacement_intent(current, enclave_signer, seeds, requested_valid_until);
    intent.operation = AttestationOperationV1::RegisterEnclave;
    intent
}

fn same_enclave_operation(
    current: &RegistrationIntentV1,
    operation: AttestationOperationV1,
) -> RegistrationIntentV1 {
    let mut intent = current.clone();
    intent.operation = operation;
    intent
}

fn strict_successor(current: &TeePolicyV1, mrenclave: B256) -> TeePolicyV1 {
    let mut successor = current.clone();
    successor.policy_version = current.policy_version + 1;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    successor.activation_height = ACTIVATION_HEIGHT;
    successor.measurement_rules[0].mrenclave = mrenclave;
    successor.measurement_rules[0].admit_from_height = ACTIVATION_HEIGHT;
    successor.measurement_rules[0].admit_until_height_exclusive = u64::MAX;
    successor
}

fn revoked_events(
    provider: &HashMapStorageProvider,
) -> Vec<alloy_primitives::Log<ITeeRegistryV1::EnclaveBindingRevokedV1>> {
    provider
        .get_ordered_events()
        .iter()
        .filter(|log| {
            log.topics().first() == Some(&ITeeRegistryV1::EnclaveBindingRevokedV1::SIGNATURE_HASH)
        })
        .map(|log| ITeeRegistryV1::EnclaveBindingRevokedV1::decode_log(log).unwrap())
        .collect()
}

fn assert_enclave_revoked(result: Result<V1RegistrationOutcome, PrecompileError>) {
    assert_revert_bytes(result, &ITeeRegistryV1::EnclaveRevoked {}.abi_encode());
}

#[test]
fn revoke_binding_makes_validator_and_node_host_not_ready() {
    let validator = LifecycleValidator::new(
        hardening_policy(B256::repeat_byte(0x59)),
        0x41,
        0x42,
        EnclaveBindingSeeds::new(0x43, 0x44),
    );
    let accepted = up_to_date_verdict_until(NOW + 20_000);
    let signed_initial = validator.signed_initial();
    let node_hash = validator.initial.node_id.node_id_hash().unwrap();
    let p2p_public = validator.initial.node_id.reth_p2p_public;
    let renew = renewal_intent(&validator.initial, NOW + 7_200);
    let same_replace = same_enclave_operation(
        &validator.initial,
        AttestationOperationV1::ReplaceEnclaveBinding,
    );
    let same_rejoin = next_binding_intent(
        &validator.initial,
        AttestationOperationV1::RegisterEnclave,
        0x91,
        NOW + 3_600,
    );
    let rejoin_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x45; 32]);
    let fresh = fresh_rejoin_intent(
        &validator.initial,
        &rejoin_enclave,
        EnclaveBindingSeeds::new(0x46, 0x47),
        NOW + 3_600,
    );
    let new_mrenclave = B256::repeat_byte(0x92);
    let successor = strict_successor(&validator.policy, new_mrenclave);
    let upgrade_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x48; 32]);
    let transition = measurement_transition_intent(
        &fresh,
        &successor,
        &upgrade_enclave,
        EnclaveBindingSeeds::new(0x49, 0x4A),
        NOW + 3_600,
    );
    let prepare =
        same_enclave_operation(&transition, AttestationOperationV1::PrepareEnclaveUpgrade);
    let final_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x4B; 32]);
    let mut final_rejoin = fresh_rejoin_intent(
        &fresh,
        &final_enclave,
        EnclaveBindingSeeds::new(0x4C, 0x4D),
        NOW + 3_600,
    );
    final_rejoin.policy_hash = successor.policy_hash().unwrap();
    let signed_renew = validator.sign(&renew);
    let signed_same_replace = validator.sign(&same_replace);
    let signed_same_rejoin = validator.sign(&same_rejoin);
    let signed_fresh = validator.sign_with_enclave(&fresh, &rejoin_enclave);
    let signed_transition = validator.sign_with_enclave(&transition, &upgrade_enclave);
    let signed_prepare = validator.sign_with_enclave(&prepare, &upgrade_enclave);
    let signed_final = validator.sign_with_enclave(&final_rejoin, &final_enclave);
    let same_rejoin_association = validator.association(&same_rejoin);
    let fresh_association = validator.association(&fresh);
    let final_association = validator.association(&final_rejoin);
    let mut upgraded = accepted.clone();
    upgraded.mrenclave = new_mrenclave;
    let first_proposal = U256::from(4);
    let upgrade_proposal = U256::from(11);

    let mut provider = validator.run_as_validator_with_binding(
        signed_initial.with_verdict(accepted.clone()),
        |_storage, mut registry| {
            let before = validator.stored_binding(&registry);
            registry
                .upgrade_candidate_context
                .write(&node_hash, B256::repeat_byte(0xC1))
                .unwrap();
            registry
                .upgrade_candidate_expiry
                .write(&node_hash, NOW + 100)
                .unwrap();
            registry
                .upgrade_candidate_source
                .write(&node_hash, before.binding_id)
                .unwrap();
            registry
                .upgrade_candidate_target
                .write(&node_hash, B256::repeat_byte(0xC2))
                .unwrap();
            registry
                .upgrade_candidate_evidence
                .write(&node_hash, B256::repeat_byte(0xC3))
                .unwrap();
            assert!(registry
                .is_validator_enclave_ready_v1(validator.node_signer.address())
                .unwrap());
            assert!(registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());
            assert_eq!(registry.last_enclave_retirement_height.read().unwrap(), 0);

            let revoked = registry
                .revoke_enclave_binding_v1(node_hash, Some(before.binding_id), first_proposal)
                .unwrap();
            assert_eq!(revoked.valid_until, 0);
            assert_eq!(revoked.enclave_id, before.enclave_id);
            assert_eq!(revoked.binding_id, before.binding_id);
            assert_eq!(revoked.intent_hash, before.intent_hash);
            let stored = validator.stored_binding(&registry);
            assert_eq!(stored.valid_until, 0);
            assert_eq!(stored.enclave_id, before.enclave_id);
            assert_eq!(stored.binding_id, before.binding_id);
            assert_eq!(stored.intent_hash, before.intent_hash);
            assert!(registry
                .v1_revoked_enclave
                .read(&before.enclave_id)
                .unwrap());
            assert!(registry
                .upgrade_candidate_context
                .read(&node_hash)
                .unwrap()
                .is_zero());
            assert_eq!(
                registry.upgrade_candidate_expiry.read(&node_hash).unwrap(),
                0
            );
            assert!(registry
                .upgrade_candidate_source
                .read(&node_hash)
                .unwrap()
                .is_zero());
            assert!(registry
                .upgrade_candidate_target
                .read(&node_hash)
                .unwrap()
                .is_zero());
            assert!(registry
                .upgrade_candidate_evidence
                .read(&node_hash)
                .unwrap()
                .is_zero());
            assert_eq!(
                registry.last_enclave_retirement_height.read().unwrap(),
                BLOCK_NUMBER
            );
            assert!(!registry
                .is_validator_enclave_ready_v1(validator.node_signer.address())
                .unwrap());
            assert!(!registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());

            assert_enclave_revoked(registry.register_enclave_after_verifier_for_test(
                signed_initial.with_verdict(accepted.clone()),
            ));
            assert_enclave_revoked(registry.renew_enclave_after_verifier_for_test(
                signed_renew.with_verdict(accepted.clone()),
            ));
            assert_enclave_revoked(registry.replace_enclave_binding_after_verifier_for_test(
                signed_same_replace.with_verdict(accepted.clone()),
            ));
            assert_enclave_revoked(registry.register_enclave_and_bind_after_verifier_for_test(
                signed_same_rejoin.with_verdict(accepted.clone()),
                same_rejoin_association.input(),
            ));
            assert_eq!(
                validator.stored_binding(&registry).binding_id,
                before.binding_id
            );

            assert_eq!(
                registry
                    .register_enclave_and_bind_after_verifier_for_test(
                        signed_fresh.with_verdict(accepted.clone()),
                        fresh_association.input(),
                    )
                    .unwrap(),
                V1RegistrationOutcome::Created
            );
            assert_eq!(
                validator.stored_binding(&registry).enclave_id,
                fresh.enclave_id
            );
            assert!(registry
                .is_validator_enclave_ready_v1(validator.node_signer.address())
                .unwrap());
            assert!(registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());
            assert!(registry
                .v1_revoked_enclave
                .read(&before.enclave_id)
                .unwrap());

            registry
                .stage_measurement_upgrade_v1(
                    upgrade_proposal,
                    new_mrenclave,
                    validator.policy.policy_hash().unwrap(),
                    ACTIVATION_HEIGHT,
                )
                .unwrap();
            let revoked_successor = registry
                .revoke_enclave_binding_v1(node_hash, Some(fresh.binding_id), upgrade_proposal)
                .unwrap();
            assert_eq!(revoked_successor.enclave_id, fresh.enclave_id);
            assert_eq!(revoked_successor.valid_until, 0);
            assert!(registry.v1_revoked_enclave.read(&fresh.enclave_id).unwrap());
            assert!(!registry
                .is_validator_enclave_ready_v1(validator.node_signer.address())
                .unwrap());
            assert!(!registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());
            assert_enclave_revoked(
                registry.transition_enclave_measurement_after_verifier_for_test(
                    validator.node_signer.address(),
                    signed_transition.with_verdict(upgraded.clone()),
                ),
            );
            assert_enclave_revoked(registry.prepare_enclave_upgrade_after_verifier_for_test(
                validator.node_signer.address(),
                signed_prepare.with_verdict(upgraded.clone()),
            ));
            assert_eq!(
                validator.stored_binding(&registry).enclave_id,
                fresh.enclave_id
            );
            assert_eq!(validator.stored_binding(&registry).valid_until, 0);
        },
    );

    let events = revoked_events(&provider);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data.nodeIdHash, node_hash);
    assert_eq!(events[0].data.bindingId, validator.initial.binding_id);
    assert_eq!(events[0].data.proposalId, first_proposal);
    assert_eq!(events[0].data.blockNumber, BLOCK_NUMBER);
    assert_eq!(events[1].data.nodeIdHash, node_hash);
    assert_eq!(events[1].data.bindingId, fresh.binding_id);
    assert_eq!(events[1].data.proposalId, upgrade_proposal);
    assert_eq!(events[1].data.blockNumber, BLOCK_NUMBER);

    provider.set_block_number(ACTIVATION_HEIGHT);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry
            .last_enclave_retirement_height
            .write(ACTIVATION_HEIGHT + 5)
            .unwrap();
        registry
            .promote_staged_successor_policy_v1(upgrade_proposal, ACTIVATION_HEIGHT)
            .unwrap();
        assert_eq!(
            registry.last_enclave_retirement_height.read().unwrap(),
            ACTIVATION_HEIGHT + 5
        );
        assert_enclave_revoked(
            registry.transition_enclave_measurement_after_verifier_for_test(
                validator.node_signer.address(),
                signed_transition.with_verdict(upgraded.clone()),
            ),
        );
        assert_enclave_revoked(registry.prepare_enclave_upgrade_after_verifier_for_test(
            validator.node_signer.address(),
            signed_prepare.with_verdict(upgraded.clone()),
        ));
        assert_eq!(
            validator.stored_binding(&registry).enclave_id,
            fresh.enclave_id
        );
        assert_eq!(validator.stored_binding(&registry).valid_until, 0);
        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test(
                    signed_final.with_verdict(upgraded),
                    final_association.input(),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        let stored = validator.stored_binding(&registry);
        assert_eq!(stored.enclave_id, final_rejoin.enclave_id);
        assert_eq!(stored.policy_hash, successor.policy_hash().unwrap());
        assert!(stored.valid_until > NOW);
        assert!(registry
            .is_validator_enclave_ready_v1(validator.node_signer.address())
            .unwrap());
        assert!(registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());
        assert!(registry
            .v1_revoked_enclave
            .read(&validator.initial.enclave_id)
            .unwrap());
        assert!(registry.v1_revoked_enclave.read(&fresh.enclave_id).unwrap());
        assert!(!registry
            .v1_revoked_enclave
            .read(&final_rejoin.enclave_id)
            .unwrap());
    });
}

#[test]
fn revoke_binding_with_mismatched_binding_id_fails_cleanly() {
    let validator = LifecycleValidator::new(
        hardening_policy(B256::repeat_byte(0x5A)),
        0x51,
        0x52,
        EnclaveBindingSeeds::new(0x53, 0x54),
    );
    let accepted = up_to_date_verdict_until(NOW + 20_000);
    let signed_initial = validator.signed_initial();
    let replacement_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x55; 32]);
    let replacement = replacement_intent(
        &validator.initial,
        &replacement_enclave,
        EnclaveBindingSeeds::new(0x56, 0x57),
        NOW + 3_600,
    );
    let signed_replacement = validator.sign_with_enclave(&replacement, &replacement_enclave);
    let node_hash = validator.initial.node_id.node_id_hash().unwrap();

    validator.run_as_validator_with_binding(
        signed_initial.with_verdict(accepted.clone()),
        |_storage, mut registry| {
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    signed_replacement.with_verdict(accepted),
                )
                .unwrap();
            let active = validator.stored_binding(&registry);
            assert_eq!(active.binding_id, replacement.binding_id);

            assert_revert_bytes(
                registry.revoke_enclave_binding_v1(
                    node_hash,
                    Some(validator.initial.binding_id),
                    U256::from(8),
                ),
                &ITeeRegistryV1::EnclaveBindingIdMismatch {}.abi_encode(),
            );
            assert_eq!(validator.stored_binding(&registry), active);
            assert!(!registry
                .v1_revoked_enclave
                .read(&active.enclave_id)
                .unwrap());

            let revoked = registry
                .revoke_enclave_binding_v1(node_hash, None, U256::from(8))
                .unwrap();
            assert_eq!(revoked.binding_id, active.binding_id);
            assert_eq!(revoked.valid_until, 0);
            assert_eq!(validator.stored_binding(&registry).valid_until, 0);
            assert!(registry
                .v1_revoked_enclave
                .read(&active.enclave_id)
                .unwrap());
        },
    );
}

#[test]
fn revoke_missing_or_expired_binding_returns_typed_error() {
    let policy = hardening_policy(B256::repeat_byte(0x5B));
    let validator = LifecycleValidator::new(
        policy.clone(),
        0x61,
        0x62,
        EnclaveBindingSeeds::new(0x63, 0x64),
    );
    let other = LifecycleValidator::new(policy, 0x65, 0x66, EnclaveBindingSeeds::new(0x67, 0x68));
    let accepted = up_to_date_verdict_until(NOW + 20_000);
    let signed_initial = validator.signed_initial();
    let signed_other = other.signed_initial();
    let other_key = [0x69; 48];

    validator.run_with_second_validator(&other.node_signer, other_key, |storage, mut registry| {
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &validator.node_signer,
            signed_initial.with_verdict(accepted.clone()),
        )
        .unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &other.node_signer,
            signed_other.with_verdict(accepted),
        )
        .unwrap();

        assert_revert_bytes(
            registry.revoke_enclave_binding_v1(B256::repeat_byte(0xEE), None, U256::from(1)),
            &ITeeRegistryV1::EnclaveBindingNotFound {}.abi_encode(),
        );

        let node_hash = validator.initial.node_id.node_id_hash().unwrap();
        registry
            .revoke_enclave_binding_v1(node_hash, None, U256::from(1))
            .unwrap();
        let revoked = validator.stored_binding(&registry);
        assert_eq!(revoked.valid_until, 0);
        assert_revert_bytes(
            registry.revoke_enclave_binding_v1(node_hash, None, U256::from(1)),
            &ITeeRegistryV1::EnclaveBindingAlreadyExpired {}.abi_encode(),
        );
        assert_eq!(validator.stored_binding(&registry), revoked);

        storage
            .set_block_timestamp(U256::from(other.initial.requested_valid_until))
            .unwrap();
        let expired_node = other.initial.node_id.node_id_hash().unwrap();
        let expired = other.stored_binding(&registry);
        assert_eq!(expired.valid_until, other.initial.requested_valid_until);
        assert_revert_bytes(
            registry.revoke_enclave_binding_v1(expired_node, None, U256::from(1)),
            &ITeeRegistryV1::EnclaveBindingAlreadyExpired {}.abi_encode(),
        );
        assert_eq!(other.stored_binding(&registry), expired);
        assert!(!registry
            .v1_revoked_enclave
            .read(&expired.enclave_id)
            .unwrap());
    });
}
