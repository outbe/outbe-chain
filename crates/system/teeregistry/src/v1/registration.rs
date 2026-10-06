use super::*;

impl TeeRegistry<'_> {
    #[cfg(feature = "tee-attestation-v1")]
    pub(super) fn require_registration_caller_v1(
        &self,
        caller: Address,
        binding: &ValidatorNodeBindingV1,
    ) -> Result<RegistrationCallerContextV1> {
        if caller != Address::from(binding.validator) {
            return Err(PrecompileError::Revert(
                "registration caller does not match the NodeHost EVM association".into(),
            ));
        }
        let associated_node = self.validator_v1_node_hash.read(&caller)?;
        let current = self.node_enclave_binding_v1(binding.node_id_hash)?;
        let Some(current) = current else {
            if !associated_node.is_zero() {
                return Err(PrecompileError::Revert(
                    "registration caller is already associated with another NodeHost".into(),
                ));
            }
            return Ok(RegistrationCallerContextV1 {
                expired_rejoin: false,
            });
        };
        if associated_node != binding.node_id_hash {
            return Err(PrecompileError::Revert(
                "registration caller is not associated with the existing NodeHost".into(),
            ));
        }
        let expired_rejoin = consensus_timestamp(&self.storage)? >= current.valid_until;
        if expired_rejoin
            && ValidatorSet::new(self.storage.clone())
                .get_validator(caller)?
                .is_some_and(|record| record.status == validator_status::JAILED)
        {
            return Err(PrecompileError::Revert(
                "jailed validator must complete ordinary unjail before enclave rejoin".into(),
            ));
        }
        Ok(RegistrationCallerContextV1 { expired_rejoin })
    }

    #[cfg(feature = "tee-attestation-v1")]
    pub(super) fn require_associated_caller_v1(
        &self,
        caller: Address,
        node_id_hash: B256,
    ) -> Result<()> {
        if self.validator_v1_node_hash.read(&caller)? != node_id_hash {
            return Err(PrecompileError::Revert(
                "TEE mutator caller is not associated with the target NodeHost".into(),
            ));
        }
        Ok(())
    }

    #[cfg(feature = "tee-attestation-v1")]
    pub(super) fn require_initial_binding_target_v1(
        intent: &RegistrationIntentV1,
        binding: &ValidatorNodeBindingV1,
    ) -> Result<()> {
        let registered_node_id_hash = intent
            .node_id
            .node_id_hash()
            .map_err(|error| revert_codec("registration NodeHost identity is invalid", error))?;
        if binding.node_id_hash != registered_node_id_hash {
            return Err(PrecompileError::Revert(
                "initial address association must reference the same NodeHost as the registration"
                    .into(),
            ));
        }
        Ok(())
    }

    #[cfg(feature = "tee-attestation-v1")]
    pub(super) fn require_initial_binding_evidence_target_v1(
        evidence: &[u8],
        binding: &ValidatorNodeBindingV1,
    ) -> Result<()> {
        let decoded = AttestationEvidenceV1::decode_canonical(evidence)
            .map_err(|error| revert_codec("attestation evidence is not canonical", error))?;
        let intent = match &decoded {
            AttestationEvidenceV1::Dcap(value) => &value.intent,
            AttestationEvidenceV1::GramineDirectDev(value) => &value.intent,
        };
        Self::require_initial_binding_target_v1(intent, binding)
    }

    /// Writes the independently authorized address-to-NodeHost association as
    /// the second half of initial registration. This function does not consult
    /// ValidatorSet membership. Key possession is not a validator role. The
    /// ordinary ValidatorSet lifecycle remains the sole owner of that role.
    pub(super) fn apply_validator_node_binding_v1(
        &mut self,
        binding: &ValidatorNodeBindingV1,
        validator_signature: &[u8; 65],
        node_signature: &[u8; 65],
    ) -> Result<V1RegistrationOutcome> {
        binding
            .validate_chain_identity(
                chain_id_word(self.storage.chain_id()?),
                self.storage.genesis_hash()?,
            )
            .map_err(|error| revert_codec("validator NodeHost binding chain mismatch", error))?;
        if !binding.verify_validator_signature(validator_signature)
            || !binding.verify_node_signature(node_signature)
        {
            return Err(PrecompileError::Revert(
                "validator NodeHost binding proof of possession is invalid".into(),
            ));
        }
        let validator = Address::from(binding.validator);
        let node = self
            .node_enclave_binding_v1(binding.node_id_hash)?
            .ok_or_else(|| {
                PrecompileError::Revert(
                    "validator NodeHost binding references an unregistered NodeHost".into(),
                )
            })?;
        if node.valid_until <= consensus_timestamp(&self.storage)? {
            return Err(PrecompileError::Revert(
                "validator NodeHost binding references an expired NodeHost".into(),
            ));
        }
        if !self.binding_code_admitted_v1(&node)? {
            return Err(PrecompileError::Revert(
                "validator NodeHost binding references a retired enclave".into(),
            ));
        }
        let current = self.validator_v1_node_hash.read(&validator)?;
        if current == binding.node_id_hash {
            return Ok(V1RegistrationOutcome::Idempotent);
        }
        if !current.is_zero() {
            return Err(PrecompileError::Revert(
                "validator is already bound to another NodeHost".into(),
            ));
        }
        self.validator_v1_node_hash
            .write(&validator, binding.node_id_hash)?;
        self.emit(ValidatorNodeHostBoundV1 {
            validator,
            nodeIdHash: binding.node_id_hash,
        })?;
        Ok(V1RegistrationOutcome::Created)
    }

    #[cfg(feature = "tee-attestation-v1")]
    pub(super) fn require_atomic_registration_outcome_v1(
        registration: V1RegistrationOutcome,
        association: V1RegistrationOutcome,
        context: RegistrationCallerContextV1,
    ) -> Result<V1RegistrationOutcome> {
        match (registration, association, context.expired_rejoin) {
            (V1RegistrationOutcome::Created, V1RegistrationOutcome::Created, false)
            | (V1RegistrationOutcome::Created, V1RegistrationOutcome::Idempotent, true)
            | (V1RegistrationOutcome::Idempotent, V1RegistrationOutcome::Idempotent, _) => {
                Ok(registration)
            }
            _ => Err(PrecompileError::Fatal(
                "NodeHost registration and address association outcomes are inconsistent".into(),
            )),
        }
    }

    /// Production verifier boundary. The transaction caller is part of the
    /// canonical NodeHost authorization and is checked before replay handling.
    pub fn register_enclave_v1(
        &mut self,
        request: EnclaveEvidenceV1<'_>,
        association: NodeHostAssociationV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        let EnclaveEvidenceV1 {
            caller,
            evidence,
            node_signature,
            enclave_signature,
        } = request;
        let NodeHostAssociationV1 {
            binding,
            validator_signature,
            node_binding_signature,
        } = association;

        let policy = self.policy_for_evidence_v1(evidence, false)?;
        self.register_enclave_with_active_policy_v1(
            EnclaveEvidenceV1 {
                caller,
                evidence,
                node_signature,
                enclave_signature,
            },
            NodeHostAssociationV1 {
                binding,
                validator_signature,
                node_binding_signature,
            },
            &policy,
        )
    }

    pub(crate) fn register_enclave_with_active_policy_v1(
        &mut self,
        request: EnclaveEvidenceV1<'_>,
        association: NodeHostAssociationV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        let EnclaveEvidenceV1 {
            caller,
            evidence,
            node_signature,
            enclave_signature,
        } = request;
        let NodeHostAssociationV1 {
            binding,
            validator_signature,
            node_binding_signature,
        } = association;

        Self::require_initial_binding_evidence_target_v1(evidence, binding)?;
        let caller_context = self.require_registration_caller_v1(caller, binding)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let registration = self.apply_evidence_mutation_with_active_policy_v1(
                AttestationOperationV1::RegisterEnclave,
                EnclaveEvidenceV1 {
                    caller,
                    evidence,
                    node_signature,
                    enclave_signature,
                },
                policy,
            )?;
            let association = self.apply_validator_node_binding_v1(
                binding,
                validator_signature,
                node_binding_signature,
            )?;
            Self::require_atomic_registration_outcome_v1(registration, association, caller_context)
        })
    }

    pub(crate) fn register_enclave_with_onboarding_v1(
        &mut self,
        request: EnclaveEvidenceV1<'_>,
        association: NodeHostAssociationV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1OnboardingOutcome> {
        let EnclaveEvidenceV1 {
            caller,
            evidence,
            node_signature,
            enclave_signature,
        } = request;
        let NodeHostAssociationV1 {
            binding,
            validator_signature,
            node_binding_signature,
        } = association;

        Self::require_initial_binding_evidence_target_v1(evidence, binding)?;
        let caller_context = self.require_registration_caller_v1(caller, binding)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let onboarding = self.apply_evidence_mutation_with_onboarding_v1(
                AttestationOperationV1::RegisterEnclave,
                EnclaveEvidenceV1 {
                    caller,
                    evidence,
                    node_signature,
                    enclave_signature,
                },
                policy,
                true,
            )?;
            let association = self.apply_validator_node_binding_v1(
                binding,
                validator_signature,
                node_binding_signature,
            )?;
            Self::require_atomic_registration_outcome_v1(
                onboarding.registration,
                association,
                caller_context,
            )?;
            Ok(onboarding)
        })
    }
}
