mod authorization;
mod commit;
mod operations;
mod ownership;
use super::*;

struct ReverseOwnersV1 {
    enclave: B256,
    binding: B256,
}

#[derive(Clone, Copy)]
struct MutationContextV1 {
    now: u64,
    node_id_hash: B256,
    intent_hash: B256,
    policy_hash: B256,
    candidate_context_hash: B256,
}

impl TeeRegistry<'_> {
    pub(super) fn apply_verified_claims_mutation_v1(
        &mut self,
        mutation: VerifiedClaimsMutationV1<'_>,
    ) -> Result<V1RegistrationOutcome> {
        self.validate_intent_envelope_v1(&mutation)?;
        let policy_hash = self.validate_mutation_policy_v1(&mutation)?;
        self.validate_mutation_possession_v1(&mutation)?;
        self.validate_mutation_claims_v1(&mutation)?;
        let context = self.mutation_context_v1(&mutation, policy_hash)?;
        if self.is_exact_mutation_replay_v1(&mutation, &context)? {
            return Ok(V1RegistrationOutcome::Idempotent);
        }
        let current = self.node_enclave_binding_v1(context.node_id_hash)?;
        self.require_existing_binding_policy_v1(&mutation, &context, &current)?;
        self.validate_mutation_operation_v1(&mutation, &context, &current)?;
        self.validate_mutation_lease_v1(&mutation, &context, &current)?;
        self.validate_mutation_ownership_v1(&mutation, &context, &current)?;
        if mutation.expected_operation == AttestationOperationV1::PrepareEnclaveUpgrade {
            return self.commit_upgrade_candidate_v1(&mutation, &context);
        }
        self.consume_upgrade_candidate_v1(&mutation, &context)?;
        self.write_active_binding_v1(&mutation, &context)?;
        self.emit_binding_mutation_v1(&mutation, &context)?;
        Ok(V1RegistrationOutcome::Created)
    }

    fn mutation_context_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        policy_hash: B256,
    ) -> Result<MutationContextV1> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            caller,
            intent,
            ..
        } = *mutation;
        let now = consensus_timestamp(&self.storage)?;
        let node_id_hash = intent
            .node_id
            .node_id_hash()
            .map_err(|error| revert_codec("node identity is invalid", error))?;
        if expected_operation != AttestationOperationV1::RegisterEnclave {
            if let Some(caller) = caller {
                self.require_associated_caller_v1(caller, node_id_hash)?;
            }
        }
        let intent_hash = intent
            .intent_hash()
            .map_err(|error| revert_codec("registration intent is invalid", error))?;
        let candidate_context_hash =
            if expected_operation == AttestationOperationV1::PrepareEnclaveUpgrade {
                let candidate_context = DcapOnboardingContextV1 {
                    chain_id: intent.chain_id,
                    genesis_hash: intent.genesis_hash,
                    intent_hash,
                    node_id_hash,
                    enclave_id: intent.enclave_id,
                    binding_id: intent.binding_id,
                    policy_hash: intent.policy_hash,
                    recipient_x25519: intent.recipient_x25519,
                    tribute_offer_public: self.offer_public_key()?.0,
                    key_epoch: self.key_epoch()?,
                    tribute_offer_epoch: self.tribute_offer_epoch()?,
                };
                candidate_context.context_hash()
            } else {
                B256::ZERO
            };
        Ok(MutationContextV1 {
            now,
            node_id_hash,
            intent_hash,
            policy_hash,
            candidate_context_hash,
        })
    }

    fn is_exact_mutation_replay_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
    ) -> Result<bool> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            evidence_hash,
            ..
        } = *mutation;
        let MutationContextV1 {
            now,
            node_id_hash,
            intent_hash,
            candidate_context_hash,
            ..
        } = *context;
        if expected_operation == AttestationOperationV1::PrepareEnclaveUpgrade
            && self.upgrade_candidate_context.read(&node_id_hash)? == candidate_context_hash
            && self.upgrade_candidate_expiry.read(&node_id_hash)? > now
            && self.upgrade_candidate_source.read(&node_id_hash)?
                == self.v1_node_binding_id.read(&node_id_hash)?
        {
            if self.upgrade_candidate_evidence.read(&node_id_hash)? != evidence_hash {
                return Err(PrecompileError::Revert(
                    "candidate is not an exact evidence replay".into(),
                ));
            }
            return Ok(true);
        }
        let current_intent_hash = self.v1_node_intent_hash.read(&node_id_hash)?;
        if !current_intent_hash.is_zero() && current_intent_hash == intent_hash {
            if self.v1_node_evidence_hash.read(&node_id_hash)? != evidence_hash {
                return Err(PrecompileError::Revert(
                    "registry mutation is not an exact evidence replay".into(),
                ));
            }
            if self.v1_node_binding_id.read(&node_id_hash)? != intent.binding_id
                || self.v1_node_enclave_id.read(&node_id_hash)? != intent.enclave_id
            {
                return Err(PrecompileError::Fatal(
                    "stored V1 idempotency identity is inconsistent".into(),
                ));
            }
            return Ok(true);
        }

        Ok(false)
    }
}
