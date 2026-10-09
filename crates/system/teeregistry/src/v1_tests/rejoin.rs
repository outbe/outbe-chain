use super::*;
use crate::v1::NodeEnclaveBindingV1;

fn new_enclave_rejoin_intent(
    current: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = replacement_intent(current, enclave_signer, seeds, requested_valid_until);
    intent.operation = AttestationOperationV1::RegisterEnclave;
    intent
}

/// The initial binding of a validator that expires: the signed initial intent
/// and the verdict accepted until `NOW + 20_000`.
struct ExpiredBinding<'a> {
    validator: &'a LifecycleValidator,
    signed_initial: SignedIntent<'a>,
    accepted: DcapVerdictV1,
}

impl<'a> ExpiredBinding<'a> {
    /// Signs the initial intent of `validator`.
    fn new(validator: &'a LifecycleValidator) -> Self {
        Self {
            validator,
            signed_initial: validator.signed_initial(),
            accepted: up_to_date_verdict_until(NOW + 20_000),
        }
    }

    /// Registers the initial binding with the accepted verdict, moves the block
    /// timestamp to the initial deadline and runs `test`.
    fn run_expired(&self, test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>)) {
        self.validator.run_with_binding_at(
            self.validator.initial.requested_valid_until,
            self.signed_initial.with_verdict(self.accepted.clone()),
            test,
        );
    }

    /// The chain of [`Self::run_expired`] without a test.
    fn expired_provider(&self) -> HashMapStorageProvider {
        self.validator.provider_with_binding_at(
            self.validator.initial.requested_valid_until,
            self.signed_initial.with_verdict(self.accepted.clone()),
        )
    }
}

/// The rejoin of a validator after its initial binding expires: the expired
/// binding, the signed rejoin and the association of the rejoin.
struct ExpiredRejoin<'a> {
    binding: ExpiredBinding<'a>,
    signed_rejoin: SignedIntent<'a>,
    association: NodeAssociation,
}

impl<'a> ExpiredRejoin<'a> {
    /// Makes the expired binding of `validator`. Then signs `rejoin` with the
    /// node key and `rejoin_enclave` and makes the association of `rejoin`.
    fn new(
        validator: &'a LifecycleValidator,
        rejoin: &'a RegistrationIntentV1,
        rejoin_enclave: &ed25519_dalek::SigningKey,
    ) -> Self {
        Self {
            binding: ExpiredBinding::new(validator),
            signed_rejoin: validator.sign_with_enclave(rejoin, rejoin_enclave),
            association: validator.association(rejoin),
        }
    }

    /// Submits the rejoin with the accepted verdict from `caller`.
    fn submit_as(
        &self,
        registry: &mut TeeRegistry<'_>,
        caller: Address,
    ) -> Result<V1RegistrationOutcome, PrecompileError> {
        registry.register_enclave_and_bind_after_verifier_for_test_as(
            caller,
            self.signed_rejoin
                .with_verdict(self.binding.accepted.clone()),
            self.association.input(),
        )
    }

    /// Submits the rejoin with the accepted verdict from the validator.
    fn submit(
        &self,
        registry: &mut TeeRegistry<'_>,
    ) -> Result<V1RegistrationOutcome, PrecompileError> {
        self.submit_as(registry, self.binding.validator.node_signer.address())
    }

    /// The `registerEnclave` calldata of the rejoin with `evidence`.
    fn register_call(&self, evidence: &[u8]) -> Vec<u8> {
        register_calldata(evidence, &self.signed_rejoin, &self.association)
    }
}

/// Asserts that the stored binding has the binding of `rejoin` and the next
/// binding and registration versions after `initial`.
fn assert_next_binding_versions(
    stored: &NodeEnclaveBindingV1,
    rejoin: &RegistrationIntentV1,
    initial: &RegistrationIntentV1,
) {
    assert_eq!(stored.binding_id, rejoin.binding_id);
    assert_eq!(stored.binding_version, initial.binding_version + 1);
    assert_eq!(
        stored.registration_version,
        initial.registration_version + 1
    );
}

/// Asserts that `registry` maps the enclave and the binding of `intent` to
/// `node_hash`.
fn assert_reverse_owner(
    registry: &TeeRegistry<'_>,
    intent: &RegistrationIntentV1,
    node_hash: B256,
) {
    assert_eq!(
        registry
            .v1_enclave_node_hash
            .read(&intent.enclave_id)
            .unwrap(),
        node_hash
    );
    assert_eq!(
        registry
            .v1_binding_node_hash
            .read(&intent.binding_id)
            .unwrap(),
        node_hash
    );
}

#[test]
fn expired_same_enclave_rejoin_is_authorized_monotonic_and_idempotent() {
    let validator = LifecycleValidator::new(
        fixed_lease_policy(B256::repeat_byte(0x32)),
        0x33,
        0x35,
        EnclaveBindingSeeds::new(0x36, 0x37),
    );
    let wrong = OutbeEvmSigner::from_secret_bytes([0x34; 32]).unwrap();
    let rejoin = next_binding_intent(
        &validator.initial,
        AttestationOperationV1::RegisterEnclave,
        0x38,
        NOW + 7_200,
    );
    let expired = ExpiredRejoin::new(&validator, &rejoin, &validator.enclave_signer);
    expired.binding.run_expired(|_storage, mut registry| {
        let node_hash = validator.initial.node_id.node_id_hash().unwrap();

        assert_reverts(expired.submit_as(&mut registry, wrong.address()), "caller");
        assert_created_then_idempotent(
            &mut registry,
            |registry| expired.submit(registry),
            |registry| {
                let stored = validator.stored_binding(registry);
                assert_next_binding_versions(&stored, &rejoin, &validator.initial);
                assert_eq!(stored.renewal_nonce, validator.initial.renewal_nonce);
                assert_eq!(stored.transition_nonce, validator.initial.transition_nonce);
                assert_eq!(stored.valid_until, NOW + 7_200);
                assert_eq!(
                    registry
                        .validator_v1_node_hash
                        .read(&validator.node_signer.address())
                        .unwrap(),
                    node_hash
                );
            },
        );
    });
}

#[test]
fn expired_new_enclave_rejoin_preserves_historical_reverse_ownership() {
    let validator = LifecycleValidator::new(
        fixed_lease_policy(B256::repeat_byte(0x39)),
        0x3A,
        0x3B,
        EnclaveBindingSeeds::new(0x3D, 0x3E),
    );
    let next_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x3C; 32]);
    let rejoin = new_enclave_rejoin_intent(
        &validator.initial,
        &next_enclave,
        EnclaveBindingSeeds::new(0x3F, 0x40),
        NOW + 7_200,
    );
    let expired = ExpiredRejoin::new(&validator, &rejoin, &next_enclave);
    expired.binding.run_expired(|_storage, mut registry| {
        let node_hash = validator.initial.node_id.node_id_hash().unwrap();

        assert_eq!(
            expired.submit(&mut registry).unwrap(),
            V1RegistrationOutcome::Created
        );
        let stored = validator.stored_binding(&registry);
        assert_eq!(stored.enclave_id, rejoin.enclave_id);
        assert_next_binding_versions(&stored, &rejoin, &validator.initial);
        assert_reverse_owner(&registry, &validator.initial, node_hash);
        assert_reverse_owner(&registry, &rejoin, node_hash);
    });
}

#[test]
fn expired_new_enclave_rejoin_abi_fits_normative_register_gas() {
    let validator = LifecycleValidator::new(
        fixed_lease_policy(B256::repeat_byte(0x52)),
        0x53,
        0x54,
        EnclaveBindingSeeds::new(0x56, 0x57),
    );
    let next_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x55; 32]);
    let rejoin = new_enclave_rejoin_intent(
        &validator.initial,
        &next_enclave,
        EnclaveBindingSeeds::new(0x58, 0x59),
        NOW + 7_200,
    );
    let expired = ExpiredRejoin::new(&validator, &rejoin, &next_enclave);
    let evidence = [0xD1; 4_096];
    let call = expired.register_call(&evidence);
    let mut provider = expired.binding.expired_provider();
    meter_production_gas(&mut provider);
    let outcome = StorageHandle::enter(&mut provider, |storage| {
        dispatch_register_after_verifier_for_test(
            storage,
            validator.node_signer.address(),
            &call,
            &rejoin,
            PostVerifierDcapCapabilityV1::new(expired.binding.accepted.clone()),
        )
        .unwrap()
    });
    assert_eq!(outcome, V1RegistrationOutcome::Created);

    assert_normative_gas(
        &provider,
        &[MeteredCall {
            kind: RegistryMutatorV1::RegisterEnclave,
            calldata: &call,
        }],
        evidence.len(),
        &validator.policy,
    );
}

#[test]
fn expired_rejoin_fails_closed_on_corrupt_current_reverse_ownership() {
    let validator = LifecycleValidator::new(
        fixed_lease_policy(B256::repeat_byte(0x5A)),
        0x5B,
        0x5C,
        EnclaveBindingSeeds::new(0x5D, 0x5E),
    );
    let rejoin = next_binding_intent(
        &validator.initial,
        AttestationOperationV1::RegisterEnclave,
        0x5F,
        NOW + 7_200,
    );
    let expired = ExpiredRejoin::new(&validator, &rejoin, &validator.enclave_signer);
    expired.binding.run_expired(|_storage, mut registry| {
        validator.assert_binding_unchanged_by(&mut registry, |registry| {
            registry
                .v1_binding_node_hash
                .write(&validator.initial.binding_id, B256::ZERO)
                .unwrap();

            assert!(matches!(
                expired.submit(registry).unwrap_err(),
                PrecompileError::Fatal(message) if message.contains("reverse ownership")
            ));
        });
    });
}

#[test]
fn expired_jailed_validator_must_unjail_before_rejoin() {
    let validator = LifecycleValidator::new(
        fixed_lease_policy(B256::repeat_byte(0x41)),
        0x42,
        0x43,
        EnclaveBindingSeeds::new(0x44, 0x45),
    );
    let rejoin = next_binding_intent(
        &validator.initial,
        AttestationOperationV1::RegisterEnclave,
        0x46,
        NOW + 7_200,
    );
    let expired = ExpiredRejoin::new(&validator, &rejoin, &validator.enclave_signer);
    validator.run_as_validator_with_binding(
        expired
            .binding
            .signed_initial
            .with_verdict(expired.binding.accepted.clone()),
        |storage, mut registry| {
            let mut validators = ValidatorSet::new(storage.clone());
            validators
                .activate_validator_via_boundary_for_test(validator.node_signer.address())
                .unwrap();
            validators
                .jail_validator(validator.node_signer.address())
                .unwrap();
            storage
                .set_block_timestamp(U256::from(validator.initial.requested_valid_until))
                .unwrap();
            validator.assert_binding_unchanged_by(&mut registry, |registry| {
                assert_reverts(expired.submit(registry), "unjail");
            });
        },
    );
}

#[test]
fn expired_binding_rejects_replace_and_transition_without_state_change() {
    let validator = LifecycleValidator::new(
        hardening_policy(B256::repeat_byte(0x47)),
        0x48,
        0x49,
        EnclaveBindingSeeds::new(0x4C, 0x4D),
    );
    let successor = measurement_successor(&validator.policy, B256::repeat_byte(0x94));
    let replacement_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x4A; 32]);
    let transition_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x4B; 32]);
    let replacement = replacement_intent(
        &validator.initial,
        &replacement_enclave,
        EnclaveBindingSeeds::new(0x4E, 0x4F),
        NOW + 7_200,
    );
    let transition = measurement_transition_intent(
        &validator.initial,
        &successor,
        &transition_enclave,
        EnclaveBindingSeeds::new(0x50, 0x51),
        NOW + 7_200,
    );
    let expired = ExpiredBinding::new(&validator);
    let signed_replacement = validator.sign_with_enclave(&replacement, &replacement_enclave);
    let signed_transition = validator.sign_with_enclave(&transition, &transition_enclave);
    let mut transition_verdict = expired.accepted.clone();
    transition_verdict.mrenclave = B256::repeat_byte(0x94);
    expired.run_expired(|_storage, mut registry| {
        registry
            .stage_successor_policy_v1(U256::from(11), &successor)
            .unwrap();

        validator.assert_binding_unchanged_by(&mut registry, |registry| {
            assert_reverts(
                registry.replace_enclave_binding_after_verifier_with_active_policy_for_test(
                    validator.node_signer.address(),
                    signed_replacement.with_verdict(expired.accepted.clone()),
                    &validator.policy,
                ),
                "expired",
            );
            assert_reverts(
                registry.transition_enclave_measurement_after_verifier_for_test(
                    validator.node_signer.address(),
                    signed_transition.with_verdict(transition_verdict),
                ),
                "expired",
            );
        });
    });
}
