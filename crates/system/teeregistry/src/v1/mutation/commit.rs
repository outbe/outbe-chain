use super::*;

impl TeeRegistry<'_> {
    pub(super) fn commit_upgrade_candidate_v1(
        &mut self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
    ) -> Result<V1RegistrationOutcome> {
        let VerifiedClaimsMutationV1 {
            intent,
            evidence_hash,
            ..
        } = *mutation;
        let MutationContextV1 {
            now,
            node_id_hash,
            candidate_context_hash,
            ..
        } = *context;

        if !self
            .upgrade_candidate_context
            .read(&node_id_hash)?
            .is_zero()
            && self.upgrade_candidate_expiry.read(&node_id_hash)? > now
        {
            return Err(PrecompileError::Revert(
                "cancel the live candidate before preparing another".into(),
            ));
        }
        self.upgrade_candidate_context
            .write(&node_id_hash, candidate_context_hash)?;
        self.upgrade_candidate_expiry
            .write(&node_id_hash, intent.requested_valid_until)?;
        self.upgrade_candidate_source
            .write(&node_id_hash, self.v1_node_binding_id.read(&node_id_hash)?)?;
        self.upgrade_candidate_target.write(
            &node_id_hash,
            intent
                .upgrade_target_hash()
                .map_err(|e| revert_codec("candidate target", e))?,
        )?;
        self.upgrade_candidate_nonce
            .write(&node_id_hash, intent.transition_nonce)?;
        self.upgrade_candidate_evidence
            .write(&node_id_hash, evidence_hash)?;
        self.upgrade_binding_requires_candidate
            .write(&intent.binding_id, true)?;
        self.emit(
            outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::EnclaveUpgradePreparedV1 {
                nodeIdHash: node_id_hash,
                contextHash: candidate_context_hash,
                bindingId: intent.binding_id,
                validUntil: intent.requested_valid_until,
                nonce: intent.transition_nonce,
            },
        )?;
        Ok(V1RegistrationOutcome::Created)
    }

    pub(super) fn consume_upgrade_candidate_v1(
        &mut self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            ..
        } = *mutation;
        let MutationContextV1 {
            now, node_id_hash, ..
        } = *context;
        if expected_operation == AttestationOperationV1::TransitionEnclaveMeasurement
            && self
                .upgrade_binding_requires_candidate
                .read(&intent.binding_id)?
            && self
                .upgrade_candidate_context
                .read(&node_id_hash)?
                .is_zero()
        {
            return Err(PrecompileError::Revert(
                "prepared upgrade binding was cancelled or consumed".into(),
            ));
        }
        if expected_operation == AttestationOperationV1::TransitionEnclaveMeasurement
            && !self
                .upgrade_candidate_context
                .read(&node_id_hash)?
                .is_zero()
        {
            self.require_live_upgrade_candidate_v1(intent, node_id_hash, now)?;
            self.clear_upgrade_candidate_v1(node_id_hash)?;
        }

        Ok(())
    }

    fn require_live_upgrade_candidate_v1(
        &self,
        intent: &RegistrationIntentV1,
        node_id_hash: B256,
        now: u64,
    ) -> Result<()> {
        if self.upgrade_candidate_expiry.read(&node_id_hash)? <= now
            || self.upgrade_candidate_source.read(&node_id_hash)?
                != self.v1_node_binding_id.read(&node_id_hash)?
            || self.upgrade_candidate_target.read(&node_id_hash)?
                != intent
                    .upgrade_target_hash()
                    .map_err(|e| revert_codec("candidate target", e))?
        {
            return Err(PrecompileError::Revert(
                "transition does not match the live upgrade candidate".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn write_active_binding_v1(
        &mut self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            intent,
            claims,
            evidence_hash,
            ..
        } = *mutation;
        let MutationContextV1 {
            now,
            node_id_hash,
            intent_hash,
            policy_hash,
            ..
        } = *context;
        let recipient = B256::from(intent.recipient_x25519);
        let attestation = B256::from(intent.attestation_ed25519);
        let noise = B256::from(intent.noise_responder_x25519);

        self.v1_node_enclave_id
            .write(&node_id_hash, intent.enclave_id)?;
        self.v1_enclave_node_hash
            .write(&intent.enclave_id, node_id_hash)?;
        self.v1_node_binding_id
            .write(&node_id_hash, intent.binding_id)?;
        self.v1_binding_node_hash
            .write(&intent.binding_id, node_id_hash)?;
        self.v1_node_intent_hash.write(&node_id_hash, intent_hash)?;
        self.v1_node_evidence_hash
            .write(&node_id_hash, evidence_hash)?;
        self.v1_node_policy_hash.write(&node_id_hash, policy_hash)?;
        self.v1_node_binding_version
            .write(&node_id_hash, intent.binding_version)?;
        self.v1_node_registration_version
            .write(&node_id_hash, intent.registration_version)?;
        self.v1_node_renewal_nonce
            .write(&node_id_hash, intent.renewal_nonce)?;
        self.v1_node_transition_nonce
            .write(&node_id_hash, intent.transition_nonce)?;
        self.v1_node_lease_started_at.write(&node_id_hash, now)?;
        self.v1_node_valid_until
            .write(&node_id_hash, intent.requested_valid_until)?;
        self.v1_node_collateral_valid_until
            .write(&node_id_hash, claims.collateral_valid_until)?;
        self.v1_node_recipient_x25519
            .write(&node_id_hash, recipient)?;
        self.v1_node_attestation_ed25519
            .write(&node_id_hash, attestation)?;
        self.v1_node_noise_responder_x25519
            .write(&node_id_hash, noise)?;
        self.v1_node_mrenclave
            .write(&node_id_hash, claims.mrenclave)?;
        self.v1_node_mrsigner
            .write(&node_id_hash, claims.mrsigner)?;
        self.v1_node_isv_prod_id
            .write(&node_id_hash, u64::from(claims.isv_prod_id))?;
        self.v1_node_isv_svn
            .write(&node_id_hash, u64::from(claims.isv_svn))?;
        self.v1_node_platform_tcb_status
            .write(&node_id_hash, u64::from(claims.platform_tcb_status))?;
        self.v1_node_verdict_hash
            .write(&node_id_hash, claims.verdict_hash)?;
        self.v1_node_host_authorization_hash.write(
            &node_id_hash,
            B256::from(intent.node_host_authorization_hash),
        )?;

        Ok(())
    }

    pub(super) fn emit_binding_mutation_v1(
        &mut self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            ..
        } = *mutation;
        let MutationContextV1 {
            node_id_hash,
            policy_hash,
            ..
        } = *context;
        match expected_operation {
            AttestationOperationV1::PrepareEnclaveUpgrade => {
                return Err(PrecompileError::Fatal(
                    "candidate crossed active-binding write boundary".into(),
                ))
            }
            AttestationOperationV1::RegisterEnclave => self.emit(EnclaveRegisteredV1 {
                nodeIdHash: node_id_hash,
                enclaveId: intent.enclave_id,
                bindingId: intent.binding_id,
                validUntil: intent.requested_valid_until,
                bindingVersion: intent.binding_version,
            })?,
            AttestationOperationV1::RenewEnclave => self.emit(EnclaveRenewedV1 {
                nodeIdHash: node_id_hash,
                enclaveId: intent.enclave_id,
                bindingId: intent.binding_id,
                validUntil: intent.requested_valid_until,
                registrationVersion: intent.registration_version,
                renewalNonce: intent.renewal_nonce,
            })?,
            AttestationOperationV1::ReplaceEnclaveBinding => {
                self.emit(EnclaveBindingReplacedV1 {
                    nodeIdHash: node_id_hash,
                    enclaveId: intent.enclave_id,
                    bindingId: intent.binding_id,
                    validUntil: intent.requested_valid_until,
                    bindingVersion: intent.binding_version,
                })?
            }
            AttestationOperationV1::TransitionEnclaveMeasurement => {
                self.emit(EnclaveMeasurementTransitionedV1 {
                    nodeIdHash: node_id_hash,
                    enclaveId: intent.enclave_id,
                    bindingId: intent.binding_id,
                    policyHash: policy_hash,
                    validUntil: intent.requested_valid_until,
                    bindingVersion: intent.binding_version,
                    transitionNonce: intent.transition_nonce,
                })?
            }
        }
        Ok(())
    }
}
