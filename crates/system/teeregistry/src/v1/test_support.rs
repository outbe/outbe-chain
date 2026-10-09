use super::*;

impl TeeRegistry<'_> {
    #[cfg(test)]
    fn apply_verified_mutation_v1(
        &mut self,
        expected_operation: AttestationOperationV1,
        caller: Option<Address>,
        verified: VerifiedIntentV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        let VerifiedIntentV1 {
            intent,
            node_signature,
            enclave_signature,
            capability,
        } = verified;

        let claims = VerifiedEnclaveClaimsV1::from_dcap(&capability.verdict)?;
        self.apply_verified_claims_mutation_v1(VerifiedClaimsMutationV1 {
            expected_operation,
            caller,
            intent,
            node_signature,
            enclave_signature,
            policy,
            claims: &claims,
            evidence_hash: capability.evidence_hash,
        })
    }

    pub(crate) fn register_enclave_after_verifier_for_test(
        &mut self,
        verified: VerifiedIntentV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_current_test_mutation_v1(AttestationOperationV1::RegisterEnclave, None, verified)
    }

    #[cfg(test)]
    pub(crate) fn register_enclave_and_bind_after_verifier_for_test(
        &mut self,
        verified: VerifiedIntentV1<'_>,
        association: NodeHostAssociationV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.register_enclave_and_bind_after_verifier_for_test_as(
            Address::from(association.binding.validator),
            verified,
            association,
        )
    }

    #[cfg(test)]
    pub(crate) fn register_enclave_and_bind_after_verifier_for_test_as(
        &mut self,
        caller: Address,
        verified: VerifiedIntentV1<'_>,
        association: NodeHostAssociationV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        let VerifiedIntentV1 {
            intent,
            node_signature,
            enclave_signature,
            capability,
        } = verified;
        let binding = association.binding;

        let policy = self.active_policy_v1()?;
        Self::require_initial_binding_target_v1(intent, binding)?;
        let caller_context = self.require_registration_caller_v1(caller, binding)?;
        self.register_and_associate_v1(association, caller_context, |registry| {
            let registration = registry.apply_verified_mutation_v1(
                AttestationOperationV1::RegisterEnclave,
                Some(caller),
                VerifiedIntentV1 {
                    intent,
                    node_signature,
                    enclave_signature,
                    capability,
                },
                &policy,
            )?;
            Ok(V1OnboardingOutcome {
                registration,
                artifact: None,
            })
        })
        .map(|outcome| outcome.registration)
    }

    pub(crate) fn renew_enclave_after_verifier_for_test(
        &mut self,
        verified: VerifiedIntentV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_current_test_mutation_v1(AttestationOperationV1::RenewEnclave, None, verified)
    }

    pub(crate) fn renew_enclave_after_verifier_for_test_as(
        &mut self,
        caller: Address,
        verified: VerifiedIntentV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_current_test_mutation_v1(
            AttestationOperationV1::RenewEnclave,
            Some(caller),
            verified,
        )
    }

    #[cfg(test)]
    pub(crate) fn renew_enclave_after_verifier_with_active_policy_for_test(
        &mut self,
        caller: Address,
        verified: VerifiedIntentV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_verified_mutation_v1(
            AttestationOperationV1::RenewEnclave,
            Some(caller),
            verified,
            policy,
        )
    }

    pub(crate) fn replace_enclave_binding_after_verifier_for_test(
        &mut self,
        verified: VerifiedIntentV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_current_test_mutation_v1(
            AttestationOperationV1::ReplaceEnclaveBinding,
            None,
            verified,
        )
    }

    #[cfg(test)]
    pub(crate) fn replace_enclave_binding_after_verifier_with_active_policy_for_test(
        &mut self,
        caller: Address,
        verified: VerifiedIntentV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_verified_mutation_v1(
            AttestationOperationV1::ReplaceEnclaveBinding,
            Some(caller),
            verified,
            policy,
        )
    }

    pub(crate) fn transition_enclave_measurement_after_verifier_for_test(
        &mut self,
        caller: Address,
        verified: VerifiedIntentV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        let (_, policy) = self
            .staged_successor_policy_v1()?
            .ok_or_else(|| PrecompileError::Revert("no successor V1 policy is staged".into()))?;
        if self.storage.block_number()? >= policy.activation_height {
            return Err(PrecompileError::Revert(
                "measurement rollout closes at successor policy activation".into(),
            ));
        }
        self.apply_verified_mutation_v1(
            AttestationOperationV1::TransitionEnclaveMeasurement,
            Some(caller),
            verified,
            &policy,
        )
    }
    fn apply_current_test_mutation_v1(
        &mut self,
        operation: AttestationOperationV1,
        caller: Option<Address>,
        verified: VerifiedIntentV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.active_policy_v1()?;
        self.apply_verified_mutation_v1(operation, caller, verified, &policy)
    }
}
