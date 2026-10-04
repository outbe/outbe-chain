use super::*;

/// Hardware-free I3 boundary: canonical ABI and full gas precharge stay real;
/// only the already-authenticated enclave outcome is supplied as a typed,
/// test-only capability.
pub(crate) fn dispatch_register_after_verifier_for_test(
    storage: StorageHandle<'_>,
    caller: Address,
    data: &[u8],
    intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    capability: crate::v1::PostVerifierDcapCapabilityV1,
) -> Result<V1RegistrationOutcome> {
    PostVerifierCall::new(storage, caller, data, intent)
        .dispatch(RegistryMutatorV1::RegisterEnclave, capability)
}

/// Hardware-free coverage of registration followed by emission of the exact
/// purpose-bound artifact returned by the verifier enclave.
pub(crate) fn dispatch_register_with_onboarding_after_verifier_for_test<F>(
    call: PostVerifierCall<'_, '_>,
    capability: crate::v1::PostVerifierDcapCapabilityV1,
    artifact_for_recipient: F,
) -> Result<V1RegistrationOutcome>
where
    F: FnOnce([u8; 32]) -> std::result::Result<Option<Vec<u8>>, String>,
{
    let onboarding_storage = call.storage.clone();
    let intent = call.intent;
    let outcome = call.dispatch(RegistryMutatorV1::RegisterEnclave, capability)?;
    let node_id_hash = intent.node_id.node_id_hash().map_err(|error| {
        PrecompileError::Revert(format!("registration node identity is invalid: {error}"))
    })?;
    let artifact = if outcome == V1RegistrationOutcome::Created {
        artifact_for_recipient(intent.recipient_x25519)
            .map_err(PrecompileError::Fatal)?
            .map(|bytes| {
                outbe_tee::dcap_protocol::DcapOnboardingArtifactV1::decode_canonical(&bytes)
                    .map_err(|code| {
                        PrecompileError::Fatal(format!(
                            "test verifier returned a non-canonical onboarding artifact: {:#06x}",
                            code.code()
                        ))
                    })
            })
            .transpose()?
    } else {
        None
    };
    TeeRegistry::new(onboarding_storage).emit_verified_onboarding_artifact_v1(
        &crate::v1::V1OnboardingOutcome {
            registration: outcome,
            artifact,
        },
        node_id_hash,
    )?;
    Ok(outcome)
}

pub(crate) fn dispatch_renew_after_verifier_for_test(
    storage: StorageHandle<'_>,
    caller: Address,
    data: &[u8],
    intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    capability: crate::v1::PostVerifierDcapCapabilityV1,
) -> Result<V1RegistrationOutcome> {
    PostVerifierCall::new(storage, caller, data, intent)
        .dispatch(RegistryMutatorV1::RenewEnclave, capability)
}

pub(crate) fn dispatch_replace_after_verifier_for_test(
    storage: StorageHandle<'_>,
    caller: Address,
    data: &[u8],
    intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    capability: crate::v1::PostVerifierDcapCapabilityV1,
) -> Result<V1RegistrationOutcome> {
    PostVerifierCall::new(storage, caller, data, intent)
        .dispatch(RegistryMutatorV1::ReplaceEnclaveBinding, capability)
}

pub(crate) fn dispatch_transition_after_verifier_for_test(
    storage: StorageHandle<'_>,
    caller: Address,
    data: &[u8],
    intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    capability: crate::v1::PostVerifierDcapCapabilityV1,
) -> Result<V1RegistrationOutcome> {
    PostVerifierCall::new(storage, caller, data, intent)
        .dispatch(RegistryMutatorV1::TransitionEnclaveMeasurement, capability)
}

/// A test invocation whose authenticated capability is supplied separately.
pub(crate) struct PostVerifierCall<'call, 'storage> {
    storage: StorageHandle<'storage>,
    caller: Address,
    data: &'call [u8],
    intent: &'call outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
}

impl<'call, 'storage> PostVerifierCall<'call, 'storage> {
    pub(crate) fn new(
        storage: StorageHandle<'storage>,
        caller: Address,
        data: &'call [u8],
        intent: &'call outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    ) -> Self {
        Self {
            storage,
            caller,
            data,
            intent,
        }
    }

    fn dispatch(
        self,
        kind: RegistryMutatorV1,
        capability: crate::v1::PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        self.prepare(kind)?.dispatch(kind, capability)
    }

    fn prepare(self, kind: RegistryMutatorV1) -> Result<PreparedVerifierCall<'call, 'storage>> {
        let Self {
            storage,
            caller,
            data,
            intent,
        } = self;
        let preflight = preflight_evidence_mutator_call(data)?;
        let policy = if kind == RegistryMutatorV1::TransitionEnclaveMeasurement {
            TeeRegistry::new(storage.clone())
                .staged_successor_policy_v1()?
                .map(|(_, policy)| policy)
                .ok_or_else(|| PrecompileError::Revert("no successor V1 policy is staged".into()))?
        } else {
            TeeRegistry::new(storage.clone()).active_policy_v1()?
        };
        deduct_mutator_protocol_gas(
            &storage,
            kind,
            data.len(),
            preflight.evidence.len(),
            &policy,
        )?;
        let (node_signature, enclave_signature) = preflight.signatures()?;
        let registry = TeeRegistry::new(storage);
        if kind == RegistryMutatorV1::TransitionEnclaveMeasurement {
            let evidence =
                AttestationEvidenceV1::decode_canonical(preflight.evidence).map_err(|error| {
                    PrecompileError::Revert(format!(
                        "attestation evidence is not canonical: {error}"
                    ))
                })?;
            registry.validate_transition_key_ready_proof_v1(&evidence)?;
        }
        Ok(PreparedVerifierCall {
            registry,
            caller,
            intent,
            preflight,
            policy,
            node_signature,
            enclave_signature,
        })
    }
}

struct PreparedVerifierCall<'call, 'storage> {
    registry: TeeRegistry<'storage>,
    caller: Address,
    intent: &'call outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    preflight: RegisterPreflight<'call>,
    policy: TeePolicyV1,
    node_signature: [u8; 65],
    enclave_signature: [u8; 64],
}

impl PreparedVerifierCall<'_, '_> {
    fn dispatch(
        self,
        kind: RegistryMutatorV1,
        capability: crate::v1::PostVerifierDcapCapabilityV1,
    ) -> Result<V1RegistrationOutcome> {
        let Self {
            mut registry,
            caller,
            intent,
            preflight,
            policy,
            node_signature,
            enclave_signature,
        } = self;
        match kind {
            RegistryMutatorV1::PrepareEnclaveUpgrade => Err(PrecompileError::Fatal(
                "prepare tests use the public verified-evidence path".into(),
            )),
            RegistryMutatorV1::RegisterEnclave => {
                let ValidatorAuthorization {
                    binding,
                    validator_signature,
                    node_binding_signature,
                } = validator_authorization(&preflight)?;
                registry.register_enclave_and_bind_after_verifier_for_test_as(
                    caller,
                    intent,
                    &node_signature,
                    &enclave_signature,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    capability,
                )
            }
            RegistryMutatorV1::RenewEnclave => registry
                .renew_enclave_after_verifier_with_active_policy_for_test(
                    caller,
                    intent,
                    &node_signature,
                    &enclave_signature,
                    &policy,
                    capability,
                ),
            RegistryMutatorV1::ReplaceEnclaveBinding => registry
                .replace_enclave_binding_after_verifier_with_active_policy_for_test(
                    caller,
                    intent,
                    &node_signature,
                    &enclave_signature,
                    &policy,
                    capability,
                ),
            RegistryMutatorV1::TransitionEnclaveMeasurement => registry
                .transition_enclave_measurement_after_verifier_for_test(
                    caller,
                    intent,
                    &node_signature,
                    &enclave_signature,
                    capability,
                ),
        }
    }
}

struct ValidatorAuthorization {
    binding: ValidatorNodeBindingV1,
    validator_signature: [u8; 65],
    node_binding_signature: [u8; 65],
}

fn validator_authorization(preflight: &RegisterPreflight<'_>) -> Result<ValidatorAuthorization> {
    let binding =
        ValidatorNodeBindingV1::decode_canonical(preflight.validator_node_binding.ok_or_else(
            || PrecompileError::Fatal("registration binding preflight was bypassed".into()),
        )?)
        .map_err(|error| {
            PrecompileError::Revert(format!(
                "validator NodeHost binding is not canonical: {error}"
            ))
        })?;
    let validator_signature = preflight
        .validator_signature
        .ok_or_else(|| {
            PrecompileError::Fatal("registration validator signature preflight was bypassed".into())
        })?
        .try_into()
        .map_err(|_| PrecompileError::Fatal("preflight validator signature mismatch".into()))?;
    let node_binding_signature = preflight
        .node_binding_signature
        .ok_or_else(|| {
            PrecompileError::Fatal(
                "registration NodeHost binding signature preflight was bypassed".into(),
            )
        })?
        .try_into()
        .map_err(|_| {
            PrecompileError::Fatal("preflight NodeHost binding signature mismatch".into())
        })?;
    Ok(ValidatorAuthorization {
        binding,
        validator_signature,
        node_binding_signature,
    })
}
