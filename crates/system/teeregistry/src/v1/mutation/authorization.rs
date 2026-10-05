use super::*;

impl TeeRegistry<'_> {
    pub(super) fn validate_intent_envelope_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            policy,
            ..
        } = *mutation;
        intent
            .encode_canonical()
            .map_err(|error| revert_codec("registration intent is not canonical", error))?;
        intent
            .validate_chain_identity(
                chain_id_word(self.storage.chain_id()?),
                self.storage.genesis_hash()?,
            )
            .map_err(|error| revert_codec("registration intent chain mismatch", error))?;
        if intent.operation != expected_operation
            || intent.attestation_mode != policy.attestation_mode
        {
            return Err(PrecompileError::Revert(
                "attestation intent operation or mode does not match the registry mutator".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_mutation_policy_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
    ) -> Result<B256> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            policy,
            ..
        } = *mutation;
        let policy_hash = policy
            .policy_hash()
            .map_err(|error| revert_codec("active V1 policy is invalid", error))?;
        if intent.policy_hash != policy_hash
            || !self
                .policy_hash_admitted_v1(policy_hash, expected_operation.is_measurement_upgrade())?
        {
            return Err(PrecompileError::Revert(
                "registration intent does not bind the authoritative V1 policy".into(),
            ));
        }
        Ok(policy_hash)
    }

    pub(super) fn validate_mutation_possession_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            intent,
            node_signature,
            enclave_signature,
            ..
        } = *mutation;
        if intent
            .derived_enclave_id()
            .map_err(|error| revert_codec("registration enclave identity is invalid", error))?
            != intent.enclave_id
        {
            return Err(PrecompileError::Revert(
                "registration enclave id is not derived from its persistent keys".into(),
            ));
        }
        if !intent.verify_node_signature(node_signature) {
            return Err(PrecompileError::Revert(
                "node proof of possession is invalid".into(),
            ));
        }
        if !intent.verify_enclave_signature(enclave_signature) {
            return Err(PrecompileError::Revert(
                "enclave proof of possession is invalid".into(),
            ));
        }

        Ok(())
    }

    pub(super) fn validate_mutation_claims_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            policy,
            claims,
            ..
        } = *mutation;
        let height = self
            .measurement_admission_height_v1(policy, expected_operation.is_measurement_upgrade())?;
        if policy.measurement_rule_match_count(
            claims.mrenclave,
            claims.mrsigner,
            claims.isv_prod_id,
            claims.isv_svn,
            height,
        ) != 1
        {
            return Err(PrecompileError::Revert(
                "verified enclave claims must match exactly one active profile measurement rule"
                    .into(),
            ));
        }
        let platform_requires_advisory_policy = matches!(
            claims.platform_tcb_status,
            status
                if status == DcapPlatformTcbStatusV1::SWHardeningNeeded as u8
                    || status
                        == DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded as u8
        );
        if policy.attestation_mode == AttestationMode::DcapRequired
            && platform_requires_advisory_policy
            && policy.accepted_platform_tcb_statuses
                != PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded
        {
            return Err(PrecompileError::Revert(
                "QVL Platform TCB status is stricter than active policy allows".into(),
            ));
        }

        Ok(())
    }
}
