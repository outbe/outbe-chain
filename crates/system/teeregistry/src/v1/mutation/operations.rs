use super::*;

fn rejoin_counters_match_v1(
    intent: &RegistrationIntentV1,
    current: &NodeEnclaveBindingV1,
) -> Result<bool> {
    if intent.binding_version != next_counter(current.binding_version, "binding version")? {
        return Ok(false);
    }
    if intent.registration_version
        != next_counter(current.registration_version, "registration version")?
    {
        return Ok(false);
    }
    Ok(intent.renewal_nonce == current.renewal_nonce
        && intent.transition_nonce == current.transition_nonce)
}

fn initial_registration_counters_match_v1(intent: &RegistrationIntentV1) -> bool {
    (
        intent.binding_version,
        intent.registration_version,
        intent.renewal_nonce,
        intent.transition_nonce,
    ) == (1, 0, 0, 0)
}

fn renewal_counters_match_v1(
    intent: &RegistrationIntentV1,
    current: &NodeEnclaveBindingV1,
) -> Result<bool> {
    Ok(intent.binding_version == current.binding_version
        && intent.registration_version
            == next_counter(current.registration_version, "registration version")?
        && intent.renewal_nonce == next_counter(current.renewal_nonce, "renewal nonce")?
        && intent.transition_nonce == current.transition_nonce)
}

fn replacement_counters_match_v1(
    intent: &RegistrationIntentV1,
    current: &NodeEnclaveBindingV1,
) -> Result<bool> {
    Ok(
        intent.binding_version == next_counter(current.binding_version, "binding version")?
            && intent.registration_version
                == next_counter(current.registration_version, "registration version")?
            && intent.renewal_nonce == current.renewal_nonce
            && intent.transition_nonce == current.transition_nonce,
    )
}

fn successor_ids_are_fresh_v1(
    intent: &RegistrationIntentV1,
    current: &NodeEnclaveBindingV1,
) -> bool {
    intent.enclave_id != current.enclave_id && intent.binding_id != current.binding_id
}

impl TeeRegistry<'_> {
    fn is_late_measurement_recovery_v1(
        &self,
        policy_hash: B256,
        current: &NodeEnclaveBindingV1,
    ) -> Result<bool> {
        let upgrade = self.enclave_upgrade_v1()?;
        Ok(!upgrade.proposal_id.is_zero()
            && self.storage.block_number()? >= upgrade.activation_height
            && policy_hash == upgrade.successor_policy_hash
            && current.policy_hash != upgrade.successor_policy_hash)
    }

    pub(super) fn require_existing_binding_policy_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            expected_operation, ..
        } = *mutation;
        let MutationContextV1 { policy_hash, .. } = *context;
        if !self.storage.enclave_upgrade_id()?.is_zero()
            && current
                .as_ref()
                .is_some_and(|binding| binding.policy_hash != policy_hash)
            && !expected_operation.is_measurement_upgrade()
        {
            return Err(PrecompileError::Revert(
                "changing an existing binding's policy requires transitionEnclaveMeasurement and its resident-key proof".into(),
            ));
        }
        Ok(())
    }

    fn validate_registration_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 { intent, .. } = *mutation;
        let MutationContextV1 { now, .. } = *context;

        if let Some(current) = current.as_ref() {
            if now < current.valid_until {
                return Err(PrecompileError::Revert(
                    "live enclave binding must renew instead of rejoin".into(),
                ));
            }
            if B256::from(intent.node_host_authorization_hash)
                != current.node_host_authorization_hash
            {
                return Err(PrecompileError::Revert(
                    "expired rejoin changes the persistent NodeHost authorization".into(),
                ));
            }
            if !rejoin_counters_match_v1(intent, current)? {
                return Err(PrecompileError::Revert(
                    "expired rejoin does not carry the exact next registration versions".into(),
                ));
            }
        } else if !initial_registration_counters_match_v1(intent) {
            return Err(PrecompileError::Revert(
                "initial registration versions and nonces are not canonical".into(),
            ));
        }
        Ok(())
    }

    fn validate_renewal_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            intent,
            policy,
            claims,
            ..
        } = *mutation;
        let MutationContextV1 { now, .. } = *context;

        let current = current.as_ref().ok_or_else(|| {
            PrecompileError::Revert("cannot renew a missing enclave binding".into())
        })?;
        ensure_continuous_binding(current, intent, claims)?;
        if !renewal_counters_match_v1(intent, current)? {
            return Err(PrecompileError::Revert(
                "renewal does not carry the exact next renewal version and nonce".into(),
            ));
        }
        ensure_renewal_window(current, policy, now)?;
        Ok(())
    }

    fn validate_replacement_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 { intent, .. } = *mutation;
        let MutationContextV1 { now, .. } = *context;

        let current = current.as_ref().ok_or_else(|| {
            PrecompileError::Revert("cannot replace a missing enclave binding".into())
        })?;
        ensure_live_binding(current, now)?;
        if B256::from(intent.node_host_authorization_hash) != current.node_host_authorization_hash {
            return Err(PrecompileError::Revert(
                "replacement changes the persistent NodeHost authorization".into(),
            ));
        }
        if !successor_ids_are_fresh_v1(intent, current) {
            return Err(PrecompileError::Revert(
                "replacement must use a fresh enclave and binding id".into(),
            ));
        }
        if !replacement_counters_match_v1(intent, current)? {
            return Err(PrecompileError::Revert(
                "replacement does not carry the exact next binding version".into(),
            ));
        }
        Ok(())
    }

    fn validate_measurement_transition_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 { intent, .. } = *mutation;
        let MutationContextV1 {
            now, policy_hash, ..
        } = *context;

        let current = current.as_ref().ok_or_else(|| {
            PrecompileError::Revert("cannot transition a missing enclave binding".into())
        })?;
        let late_recovery = self.is_late_measurement_recovery_v1(policy_hash, current)?;
        if !late_recovery {
            ensure_live_binding(current, now)?;
        }
        if B256::from(intent.node_host_authorization_hash) != current.node_host_authorization_hash {
            return Err(PrecompileError::Revert(
                "measurement transition changes the persistent NodeHost authorization".into(),
            ));
        }
        if !successor_ids_are_fresh_v1(intent, current) {
            return Err(PrecompileError::Revert(
                "measurement transition must use a fresh enclave and binding id".into(),
            ));
        }
        if intent.binding_version != next_counter(current.binding_version, "binding version")?
            || self.transition_counters_mismatch_v1(mutation, context, current)?
        {
            return Err(PrecompileError::Revert(
                "measurement transition does not carry the exact next versions and nonce".into(),
            ));
        }
        Ok(())
    }

    fn transition_counters_mismatch_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &NodeEnclaveBindingV1,
    ) -> Result<bool> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            ..
        } = *mutation;
        let node_id_hash = context.node_id_hash;
        Ok(
            (expected_operation != AttestationOperationV1::PrepareEnclaveUpgrade
                && (intent.registration_version
                    != next_counter(current.registration_version, "registration version")?
                    || intent.renewal_nonce != current.renewal_nonce))
                || intent.transition_nonce
                    != if expected_operation == AttestationOperationV1::PrepareEnclaveUpgrade {
                        next_counter(
                            self.upgrade_candidate_nonce.read(&node_id_hash)?,
                            "candidate nonce",
                        )?
                    } else {
                        next_counter(current.transition_nonce, "transition nonce")?
                    },
        )
    }

    pub(super) fn validate_mutation_operation_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        match mutation.expected_operation {
            AttestationOperationV1::RegisterEnclave => {
                self.validate_registration_v1(mutation, context, current)
            }
            AttestationOperationV1::RenewEnclave => {
                self.validate_renewal_v1(mutation, context, current)
            }
            AttestationOperationV1::ReplaceEnclaveBinding => {
                self.validate_replacement_v1(mutation, context, current)
            }
            AttestationOperationV1::TransitionEnclaveMeasurement
            | AttestationOperationV1::PrepareEnclaveUpgrade => {
                self.validate_measurement_transition_v1(mutation, context, current)
            }
        }
    }

    pub(super) fn validate_mutation_lease_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            policy,
            claims,
            ..
        } = *mutation;
        let MutationContextV1 { now, .. } = *context;
        if expected_operation == AttestationOperationV1::RenewEnclave {
            let current = current.as_ref().ok_or_else(|| {
                PrecompileError::Fatal("renewal binding disappeared during validation".into())
            })?;
            let expected_deadline = current
                .valid_until
                .checked_add(policy.maximum_lease)
                .ok_or_else(|| PrecompileError::Revert("renewal deadline overflows u64".into()))?;
            if intent.requested_valid_until != expected_deadline {
                return Err(PrecompileError::Revert(
                    "renewal must extend exactly one lease period from the current deadline".into(),
                ));
            }
        } else {
            let lease = intent
                .requested_valid_until
                .checked_sub(now)
                .ok_or_else(|| {
                    PrecompileError::Revert("requested lease is already expired".into())
                })?;
            if lease < policy.minimum_lease || lease > policy.maximum_lease {
                return Err(PrecompileError::Revert(
                    "requested lease is outside active policy bounds".into(),
                ));
            }
        }
        if policy.attestation_mode == AttestationMode::DcapRequired {
            let collateral_limit = claims
                .collateral_valid_until
                .checked_sub(policy.collateral_margin)
                .ok_or_else(|| {
                    PrecompileError::Revert(
                        "verified collateral leaves no mandatory safety margin".into(),
                    )
                })?;
            if intent.requested_valid_until > collateral_limit {
                return Err(PrecompileError::Revert(
                    "requested lease exceeds verified collateral validity".into(),
                ));
            }
        }
        Ok(())
    }
}
