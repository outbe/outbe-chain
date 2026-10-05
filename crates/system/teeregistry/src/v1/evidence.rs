use super::*;

struct VerifiedDcapClaimsV1 {
    claims: VerifiedEnclaveClaimsV1,
    evidence_hash: B256,
    artifact: Option<DcapOnboardingArtifactV1>,
}

impl TeeRegistry<'_> {
    /// Emit only the artifact produced by the same purpose-bound enclave
    /// verification capability that accepted this registration. No second raw
    /// host-selected sealing request exists on the production path.
    pub(crate) fn emit_verified_onboarding_artifact_v1(
        &mut self,
        outcome: &V1OnboardingOutcome,
        node_id_hash: B256,
    ) -> Result<()> {
        if outcome.registration == V1RegistrationOutcome::Idempotent {
            return Ok(());
        }
        let artifact = outcome.artifact.as_ref().ok_or_else(|| {
            PrecompileError::Fatal(
                "created registration has no purpose-bound onboarding artifact".into(),
            )
        })?;
        let context = artifact.context;
        let expected_offer_public = self.offer_public_key()?;
        if expected_offer_public.is_zero()
            || !self.onboarding_identity_matches_v1(&context, node_id_hash)?
            || !self.onboarding_offer_matches_v1(&context, expected_offer_public)?
            || !self.onboarding_binding_matches_v1(&context, node_id_hash)?
        {
            return Err(PrecompileError::Fatal(
                "purpose-bound onboarding artifact does not match committed Registry binding"
                    .into(),
            ));
        }
        let encoded = artifact.encode_canonical().map_err(|code| {
            PrecompileError::Fatal(format!(
                "purpose-bound onboarding artifact is non-canonical: {:#06x}",
                code.code()
            ))
        })?;
        self.emit(OfferKeySealedForRegistryV1 {
            nodeIdHash: node_id_hash,
            sealedOfferKey: encoded.into(),
        })
    }

    pub fn renew_enclave_v1(
        &mut self,
        request: EnclaveEvidenceV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_current_evidence_mutation_v1(AttestationOperationV1::RenewEnclave, request)
    }

    pub fn replace_enclave_binding_v1(
        &mut self,
        request: EnclaveEvidenceV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_current_evidence_mutation_v1(
            AttestationOperationV1::ReplaceEnclaveBinding,
            request,
        )
    }

    pub(crate) fn renew_enclave_with_active_policy_v1(
        &mut self,
        request: EnclaveEvidenceV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_evidence_mutation_with_active_policy_v1(
            AttestationOperationV1::RenewEnclave,
            request,
            policy,
        )
    }

    pub(crate) fn replace_enclave_binding_with_active_policy_v1(
        &mut self,
        request: EnclaveEvidenceV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_evidence_mutation_with_active_policy_v1(
            AttestationOperationV1::ReplaceEnclaveBinding,
            request,
            policy,
        )
    }

    pub(crate) fn prepare_enclave_upgrade_v1(
        &mut self,
        caller: Address,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.policy_for_evidence_v1(evidence, true)?;
        let upgrade = self.enclave_upgrade_v1()?;
        if upgrade.proposal_id.is_zero()
            || policy
                .policy_hash()
                .map_err(|e| revert_codec("upgrade policy", e))?
                != upgrade.successor_policy_hash
        {
            return Err(PrecompileError::Revert(
                "candidate requires an approved MRENCLAVE upgrade".into(),
            ));
        }
        self.apply_evidence_mutation_with_active_policy_v1(
            AttestationOperationV1::PrepareEnclaveUpgrade,
            EnclaveEvidenceV1 {
                caller,
                evidence,
                node_signature,
                enclave_signature,
            },
            &policy,
        )
    }

    pub(crate) fn cancel_enclave_upgrade_v1(
        &mut self,
        caller: Address,
        node: B256,
        expected: B256,
    ) -> Result<()> {
        if self.storage.is_static()? {
            return Err(PrecompileError::WriteProtection);
        }
        self.require_associated_caller_v1(caller, node)?;
        if expected.is_zero() || self.upgrade_candidate_context.read(&node)? != expected {
            return Err(PrecompileError::Revert(
                "pending upgrade changed or is absent".into(),
            ));
        }
        self.clear_upgrade_candidate_v1(node)?;
        self.emit(
            outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::EnclaveUpgradeCancelledV1 {
                nodeIdHash: node,
                contextHash: expected,
            },
        )
    }

    pub(super) fn clear_upgrade_candidate_v1(&mut self, node: B256) -> Result<()> {
        self.upgrade_candidate_context.write(&node, B256::ZERO)?;
        self.upgrade_candidate_expiry.write(&node, 0)?;
        self.upgrade_candidate_source.write(&node, B256::ZERO)?;
        self.upgrade_candidate_target.write(&node, B256::ZERO)?;
        self.upgrade_candidate_evidence.write(&node, B256::ZERO)
    }

    pub(crate) fn transition_enclave_measurement_with_staged_policy_v1(
        &mut self,
        caller: Address,
        evidence: &[u8],
        node_signature: &[u8; 65],
        enclave_signature: &[u8; 64],
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.policy_for_evidence_v1(evidence, true)?;
        if self.storage.enclave_upgrade_id()?.is_zero()
            && self.storage.block_number()? >= policy.activation_height
        {
            return Err(PrecompileError::Revert(
                "measurement rollout closes at successor policy activation".into(),
            ));
        }
        self.apply_evidence_mutation_with_active_policy_v1(
            AttestationOperationV1::TransitionEnclaveMeasurement,
            EnclaveEvidenceV1 {
                caller,
                evidence,
                node_signature,
                enclave_signature,
            },
            &policy,
        )
    }

    pub(super) fn apply_evidence_mutation_with_active_policy_v1(
        &mut self,
        expected_operation: AttestationOperationV1,
        request: EnclaveEvidenceV1<'_>,
        policy: &TeePolicyV1,
    ) -> Result<V1RegistrationOutcome> {
        self.apply_evidence_mutation_with_onboarding_v1(expected_operation, request, policy, false)
            .map(|outcome| outcome.registration)
    }

    pub(super) fn apply_evidence_mutation_with_onboarding_v1(
        &mut self,
        expected_operation: AttestationOperationV1,
        request: EnclaveEvidenceV1<'_>,
        policy: &TeePolicyV1,
        issue_onboarding_artifact: bool,
    ) -> Result<V1OnboardingOutcome> {
        let EnclaveEvidenceV1 {
            caller,
            evidence,
            node_signature,
            enclave_signature,
        } = request;

        let decoded = AttestationEvidenceV1::decode_canonical(evidence)
            .map_err(|error| revert_codec("attestation evidence is not canonical", error))?;
        if expected_operation == AttestationOperationV1::TransitionEnclaveMeasurement {
            self.validate_transition_key_ready_proof_v1(&decoded)?;
        }
        if decoded.mode() != policy.attestation_mode {
            return Err(PrecompileError::Revert(
                "attestation evidence mode does not match the active V1 policy".into(),
            ));
        }

        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
        let (intent, claims, evidence_hash, onboarding_artifact) = match &decoded {
            AttestationEvidenceV1::Dcap(dcap) => {
                let verified = self.verify_dcap_claims_v1(
                    expected_operation,
                    request,
                    policy,
                    issue_onboarding_artifact,
                )?;
                (
                    dcap.intent.clone(),
                    verified.claims,
                    verified.evidence_hash,
                    verified.artifact,
                )
            }
            AttestationEvidenceV1::GramineDirectDev(dev) => {
                let (claims, evidence_hash) = self.verify_dev_claims_v1(
                    dev,
                    &decoded,
                    policy,
                    expected_operation,
                    enclave_signature,
                )?;
                (dev.intent.clone(), claims, evidence_hash, None)
            }
        };
        let registration = self.apply_verified_claims_mutation_v1(VerifiedClaimsMutationV1 {
            expected_operation,
            caller: Some(caller),
            intent: &intent,
            node_signature,
            enclave_signature,
            policy,
            claims: &claims,
            evidence_hash,
        })?;
        let artifact = self.complete_onboarding_artifact_v1(
            expected_operation,
            &intent,
            policy,
            V1OnboardingOutcome {
                registration,
                artifact: onboarding_artifact,
            },
            issue_onboarding_artifact,
        )?;
        Ok(V1OnboardingOutcome {
            registration,
            artifact,
        })
    }

    pub(crate) fn validate_transition_key_ready_proof_v1(
        &self,
        evidence: &AttestationEvidenceV1,
    ) -> Result<()> {
        if evidence.mode() == AttestationMode::GramineDirectDev
            && self.enclave_upgrade_v1()?.proposal_id.is_zero()
        {
            return Err(PrecompileError::Revert(
                "DirectDev transition requires a MRENCLAVE upgrade".into(),
            ));
        }
        let proof = evidence.transition_key_ready_proof().ok_or_else(|| {
            PrecompileError::Revert("measurement transition is missing key-ready proof".into())
        })?;
        let offer_public = self.offer_public_key()?;
        if offer_public.is_zero() {
            return Err(PrecompileError::Fatal(
                "measurement transition requires the permanent offer-key commitment".into(),
            ));
        }
        proof
            .verify_for_transition(evidence.intent(), offer_public.0)
            .map_err(|error| {
                PrecompileError::Revert(format!(
                    "measurement transition key-ready proof is invalid: {error}"
                ))
            })
    }
    fn apply_current_evidence_mutation_v1(
        &mut self,
        operation: AttestationOperationV1,
        request: EnclaveEvidenceV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        let policy = self.policy_for_evidence_v1(request.evidence, false)?;
        self.apply_evidence_mutation_with_active_policy_v1(operation, request, &policy)
    }
    fn verify_dcap_claims_v1(
        &self,
        expected_operation: AttestationOperationV1,
        request: EnclaveEvidenceV1<'_>,
        policy: &TeePolicyV1,
        issue_onboarding_artifact: bool,
    ) -> Result<VerifiedDcapClaimsV1> {
        let EnclaveEvidenceV1 {
            evidence,
            node_signature,
            enclave_signature,
            ..
        } = request;
        let policy_bytes = policy.encode_canonical().map_err(|error| {
            PrecompileError::Fatal(format!("active V1 policy cannot be encoded: {error}"))
        })?;
        let consensus_timestamp = consensus_timestamp(&self.storage)?;
        let (outcome, onboarding_artifact) = if issue_onboarding_artifact {
            if expected_operation != AttestationOperationV1::RegisterEnclave {
                return Err(PrecompileError::Fatal(
                    "onboarding artifact requested for a non-registration operation".into(),
                ));
            }
            let offer_public = self.offer_public_key()?;
            if offer_public.is_zero() {
                return Err(PrecompileError::Fatal(
                    "DcapRequired registration requires the OST3 offer-key commitment".into(),
                ));
            }
            let result = outbe_tee::verify_dcap_registration_and_seal_v1(
                        evidence,
                        &policy_bytes,
                        consensus_timestamp,
                        node_signature,
                        enclave_signature,
                        offer_public.0,
                        self.key_epoch()?,
                        self.tribute_offer_epoch()?,
                    )
                    .map_err(|error| {
                        PrecompileError::Fatal(format!(
                            "purpose-bound DCAP onboarding verifier is unavailable or unauthenticated: {error}"
                        ))
                    })?;
            (result.outcome, result.artifact)
        } else {
            (
                        outbe_tee::verify_dcap_evidence_v1(
                            evidence,
                            &policy_bytes,
                            consensus_timestamp,
                        )
                        .map_err(|error| {
                            PrecompileError::Fatal(format!(
                                "enclave-resident DCAP verifier is unavailable or unauthenticated: {error}"
                            ))
                        })?,
                        None,
                    )
        };
        let verdict = match outcome {
            DcapVerificationOutcomeV1::Accepted(verdict) => verdict,
            DcapVerificationOutcomeV1::Rejected(code) => {
                return Err(PrecompileError::Revert(format!(
                    "DCAP evidence rejected with code {:#06x}",
                    code.code()
                )))
            }
        };
        let evidence_hash = dcap_evidence_hash_v1(evidence).map_err(|code| {
            PrecompileError::Fatal(format!(
                "accepted DCAP evidence cannot be hashed: {:#06x}",
                code.code()
            ))
        })?;
        Ok(VerifiedDcapClaimsV1 {
            claims: VerifiedEnclaveClaimsV1::from_dcap(&verdict)?,
            evidence_hash,
            artifact: onboarding_artifact,
        })
    }
    fn verify_dev_claims_v1(
        &self,
        dev: &outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1,
        decoded: &AttestationEvidenceV1,
        policy: &TeePolicyV1,
        expected_operation: AttestationOperationV1,
        enclave_signature: &[u8; 64],
    ) -> Result<(VerifiedEnclaveClaimsV1, B256)> {
        if dev.dev_attestation_public != dev.intent.attestation_ed25519
            || dev.dev_signature != *enclave_signature
            || !dev.intent.verify_enclave_signature(&dev.dev_signature)
        {
            return Err(PrecompileError::Revert(
                "GramineDirectDev evidence signature does not bind the registration intent".into(),
            ));
        }
        let evidence_hash = decoded
            .evidence_hash()
            .map_err(|error| revert_codec("GramineDirectDev evidence is not canonical", error))?;
        let height = self
            .measurement_admission_height_v1(policy, expected_operation.is_measurement_upgrade())?;
        let claims = direct_dev_claims(policy, height, evidence_hash)?;
        Ok((claims, evidence_hash))
    }
    fn complete_onboarding_artifact_v1(
        &self,
        expected_operation: AttestationOperationV1,
        intent: &RegistrationIntentV1,
        policy: &TeePolicyV1,
        outcome: V1OnboardingOutcome,
        issue_onboarding_artifact: bool,
    ) -> Result<Option<DcapOnboardingArtifactV1>> {
        let V1OnboardingOutcome {
            registration,
            artifact: onboarding_artifact,
        } = outcome;
        let artifact = if onboarding_artifact.is_some()
            || !issue_onboarding_artifact
            || registration == V1RegistrationOutcome::Idempotent
        {
            onboarding_artifact
        } else if policy.attestation_mode == AttestationMode::GramineDirectDev {
            if expected_operation != AttestationOperationV1::RegisterEnclave {
                return Err(PrecompileError::Fatal(
                    "onboarding artifact requested for a non-registration operation".into(),
                ));
            }
            let offer_public = self.offer_public_key()?;
            if offer_public.is_zero() {
                return Err(PrecompileError::Fatal(
                    "GramineDirectDev registration requires the OST3 offer-key commitment".into(),
                ));
            }
            let context = DcapOnboardingContextV1 {
                chain_id: intent.chain_id,
                genesis_hash: intent.genesis_hash,
                intent_hash: intent.intent_hash().map_err(|error| {
                    revert_codec("GramineDirectDev registration intent is invalid", error)
                })?,
                node_id_hash: intent.node_id.node_id_hash().map_err(|error| {
                    revert_codec("GramineDirectDev node identity is invalid", error)
                })?,
                enclave_id: intent.enclave_id,
                binding_id: intent.binding_id,
                policy_hash: intent.policy_hash,
                recipient_x25519: intent.recipient_x25519,
                tribute_offer_public: offer_public.0,
                key_epoch: self.key_epoch()?,
                tribute_offer_epoch: self.tribute_offer_epoch()?,
            };
            let _enclave_context =
                outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
            Some(
                outbe_tee::prepare_gramine_direct_dev_onboarding_artifact_v1(context).map_err(
                    |error| {
                        PrecompileError::Fatal(format!(
                            "purpose-bound GramineDirectDev onboarding artifact failed: {error}"
                        ))
                    },
                )?,
            )
        } else {
            return Err(PrecompileError::Fatal(
                "created registration has no purpose-bound onboarding artifact".into(),
            ));
        };
        Ok(artifact)
    }
    fn onboarding_identity_matches_v1(
        &self,
        context: &DcapOnboardingContextV1,
        node_id_hash: B256,
    ) -> Result<bool> {
        Ok(context.chain_id == chain_id_word(self.storage.chain_id()?)
            && context.genesis_hash == self.storage.genesis_hash()?
            && context.node_id_hash == node_id_hash)
    }
    fn onboarding_offer_matches_v1(
        &self,
        context: &DcapOnboardingContextV1,
        expected_offer_public: B256,
    ) -> Result<bool> {
        Ok(context.tribute_offer_public == expected_offer_public.0
            && context.key_epoch == self.key_epoch()?
            && context.tribute_offer_epoch == self.tribute_offer_epoch()?)
    }
    fn onboarding_binding_matches_v1(
        &self,
        context: &DcapOnboardingContextV1,
        node_id_hash: B256,
    ) -> Result<bool> {
        Ok(
            self.v1_node_intent_hash.read(&node_id_hash)? == context.intent_hash
                && self.v1_node_enclave_id.read(&node_id_hash)? == context.enclave_id
                && self.v1_node_recipient_x25519.read(&node_id_hash)?
                    == B256::from(context.recipient_x25519),
        )
    }
}
