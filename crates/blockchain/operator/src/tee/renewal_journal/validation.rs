use super::*;

struct DecodedIntent<'a> {
    intent: RegistrationIntentV1,
    intent_hash: B256,
    node_signature: &'a [u8; 65],
    enclave_signature: &'a [u8; 64],
}
struct EvidenceCommitment {
    evidence: AttestationEvidenceV1,
    hash: B256,
}
impl PreparedRenewalV1 {
    pub(super) fn validate(&self) -> Result<()> {
        self.validate_bounds()?;
        let decoded = self.decode_intent()?;
        self.validate_exact_next_intent(&decoded)?;
        let evidence = self.validate_evidence(&decoded.intent)?;
        self.validate_commitments(&decoded, &evidence)?;
        self.validate_calldata()?;
        self.validate_relays()?;
        self.validate_collateral(&evidence.evidence)
    }
    fn validate_bounds(&self) -> Result<()> {
        if !self.has_encoded_material() || !self.has_bounded_signatures_and_variants() {
            return Err(RenewalJournalError::InvalidBoundedMaterial.into());
        }
        Ok(())
    }
    fn has_encoded_material(&self) -> bool {
        !self.intent.is_empty() && !self.evidence.is_empty() && !self.calldata.is_empty()
    }
    fn has_bounded_signatures_and_variants(&self) -> bool {
        self.node_signature.len() == 65
            && self.enclave_signature.len() == 64
            && !self.relay_variants.is_empty()
            && self.relay_variants.len() <= MAX_RELAY_VARIANTS
    }
    fn decode_intent(&self) -> Result<DecodedIntent<'_>> {
        let intent = RegistrationIntentV1::decode_canonical(&self.intent)
            .map_err(RenewalJournalError::DecodeIntent)?;
        let intent_hash = intent
            .intent_hash()
            .map_err(RenewalJournalError::HashIntent)?;
        let node_signature: &[u8; 65] = self
            .node_signature
            .as_slice()
            .try_into()
            .map_err(|_| RenewalJournalError::InvalidNodeSignatureLength)?;
        let enclave_signature: &[u8; 64] = self
            .enclave_signature
            .as_slice()
            .try_into()
            .map_err(|_| RenewalJournalError::InvalidEnclaveSignatureLength)?;
        Ok(DecodedIntent {
            intent,
            intent_hash,
            node_signature,
            enclave_signature,
        })
    }
    fn validate_exact_next_intent(&self, decoded: &DecodedIntent<'_>) -> Result<()> {
        let intent = &decoded.intent;
        let next_registration_version = self
            .source
            .registration_version
            .checked_add(1)
            .ok_or(RenewalJournalError::SourceRegistrationVersionExhausted)?;
        let next_renewal_nonce = self
            .source
            .renewal_nonce
            .checked_add(1)
            .ok_or(RenewalJournalError::SourceNonceExhausted)?;
        let node_id_hash = intent
            .node_id
            .node_id_hash()
            .map_err(RenewalJournalError::HashNodeHostIdentity)?;
        let derived_enclave_id = intent
            .derived_enclave_id()
            .map_err(RenewalJournalError::DeriveEnclaveIdentity)?;
        if intent.operation != AttestationOperationV1::RenewEnclave
            || node_id_hash != self.source.node_id_hash
            || intent.enclave_id != self.source.enclave_id
            || derived_enclave_id != intent.enclave_id
        {
            return Err(RenewalJournalError::NotExactNextBindingTransition.into());
        }
        if intent.binding_id != self.source.binding_id
            || intent.policy_hash != self.source.policy_hash
            || intent.binding_version != self.source.binding_version
        {
            return Err(RenewalJournalError::NotExactNextBindingTransition.into());
        }
        if intent.registration_version != next_registration_version
            || intent.renewal_nonce != next_renewal_nonce
            || intent.transition_nonce != self.source.transition_nonce
            || intent.requested_valid_until != self.requested_valid_until
        {
            return Err(RenewalJournalError::NotExactNextBindingTransition.into());
        }
        if B256::from(intent.recipient_x25519) != self.source.recipient_x25519
            || B256::from(intent.attestation_ed25519) != self.source.attestation_ed25519
            || B256::from(intent.noise_responder_x25519) != self.source.noise_responder_x25519
            || intent.node_host_authorization_hash != self.source.node_host_authorization_hash
        {
            return Err(RenewalJournalError::NotExactNextBindingTransition.into());
        }
        if !intent.verify_node_signature(decoded.node_signature)
            || !intent.verify_enclave_signature(decoded.enclave_signature)
        {
            return Err(RenewalJournalError::NotExactNextBindingTransition.into());
        }
        Ok(())
    }
    fn validate_evidence(&self, intent: &RegistrationIntentV1) -> Result<EvidenceCommitment> {
        let evidence = AttestationEvidenceV1::decode_canonical(&self.evidence)
            .map_err(RenewalJournalError::DecodeEvidence)?;
        let evidence_hash = match &evidence {
            AttestationEvidenceV1::Dcap(_) => {
                if intent.attestation_mode != AttestationMode::DcapRequired {
                    return Err(RenewalJournalError::EvidenceModeMismatch.into());
                }
                dcap_evidence_hash_v1(&self.evidence)
                    .map_err(RenewalJournalError::HashDcapEvidence)?
            }
            AttestationEvidenceV1::GramineDirectDev(value) => {
                let direct_binding_matches = intent.attestation_mode
                    == AttestationMode::GramineDirectDev
                    && value.dev_attestation_public == value.intent.attestation_ed25519
                    && value.dev_signature.as_slice() == self.enclave_signature.as_slice();
                if !direct_binding_matches
                    || !value.intent.verify_enclave_signature(&value.dev_signature)
                {
                    return Err(RenewalJournalError::InvalidDirectDevEvidence.into());
                }
                evidence
                    .evidence_hash()
                    .map_err(RenewalJournalError::HashEvidence)?
            }
        };
        Ok(EvidenceCommitment {
            evidence,
            hash: evidence_hash,
        })
    }
    fn validate_commitments(
        &self,
        decoded: &DecodedIntent<'_>,
        evidence: &EvidenceCommitment,
    ) -> Result<()> {
        let intent_matches = decoded.intent_hash == self.intent_hash
            && evidence.evidence.intent() == &decoded.intent;
        if !intent_matches
            || evidence.hash != self.evidence_hash
            || keccak256(&self.calldata) != self.calldata_hash
        {
            return Err(RenewalJournalError::HashCommitmentMismatch.into());
        }
        Ok(())
    }
    fn validate_calldata(&self) -> Result<()> {
        let canonical_calldata = ITeeRegistryV1::renewEnclaveCall {
            evidence: self.evidence.clone().into(),
            nodeSignature: self.node_signature.clone().into(),
            enclaveSignature: self.enclave_signature.clone().into(),
        }
        .abi_encode();
        if self.calldata != canonical_calldata {
            return Err(RenewalJournalError::NonCanonicalCalldata.into());
        }
        Ok(())
    }
    fn validate_relays(&self) -> Result<()> {
        let first = &self.relay_variants[0];
        if first.relay != self.relay || first.calldata_hash != self.calldata_hash {
            return Err(RenewalJournalError::RelayBindingMismatch.into());
        }
        for variant in &self.relay_variants {
            if !relay_identity_matches(variant, first, self.relay)
                || variant.calldata_hash != self.calldata_hash
                || keccak256(&variant.raw_transaction) != variant.transaction_hash
            {
                return Err(RenewalJournalError::CompetingRelayVariant.into());
            }
        }
        Ok(())
    }
    fn validate_collateral(&self, evidence: &AttestationEvidenceV1) -> Result<()> {
        match evidence {
            AttestationEvidenceV1::Dcap(_) => {
                let ceiling = self
                    .collateral_valid_until
                    .checked_sub(self.collateral_margin)
                    .ok_or(RenewalJournalError::CollateralMarginUnderflow)?;
                if self.requested_valid_until > ceiling {
                    return Err(RenewalJournalError::LeaseExceedsCollateralCeiling.into());
                }
            }
            AttestationEvidenceV1::GramineDirectDev(_) => {
                if self.collateral_valid_until != u64::MAX || self.collateral_margin != 0 {
                    return Err(RenewalJournalError::NonCanonicalDirectDevCollateral.into());
                }
            }
        }
        Ok(())
    }
}

fn relay_identity_matches(
    variant: &RawRelayTransactionV1,
    first: &RawRelayTransactionV1,
    relay: Address,
) -> bool {
    variant.relay == relay
        && variant.chain_id == first.chain_id
        && variant.account_nonce == first.account_nonce
        && variant.gas_limit == first.gas_limit
}

impl RenewalJournalStateV1 {
    pub(super) fn validate(&self) -> Result<()> {
        self.attempt().validate()?;
        self.validate_submitted()?;
        self.validate_finalized()?;
        self.validate_abandoned()
    }
    fn validate_submitted(&self) -> Result<()> {
        if let Self::Submitted {
            attempt,
            transaction_hashes,
            ..
        } = self
        {
            if transaction_hashes.is_empty()
                || transaction_hashes.len() > attempt.relay_variants.len()
                || transaction_hashes
                    .iter()
                    .enumerate()
                    .any(|(index, hash)| attempt.relay_variants[index].transaction_hash != *hash)
            {
                return Err(RenewalJournalError::NonCanonicalSubmittedTransactionList.into());
            }
        }
        Ok(())
    }
    fn validate_finalized(&self) -> Result<()> {
        if let Self::Finalized {
            attempt,
            finalized_binding,
            finalized_hash,
            ..
        } = self
        {
            if finalized_hash.is_zero()
                || finalized_binding.intent_hash != attempt.intent_hash
                || finalized_binding.evidence_hash != attempt.evidence_hash
            {
                return Err(RenewalJournalError::FinalizedBindingMismatch.into());
            }
        }
        Ok(())
    }
    fn validate_abandoned(&self) -> Result<()> {
        if let Self::Abandoned { reason, .. } = self {
            if reason.is_empty() || reason.len() > 512 {
                return Err(RenewalJournalError::InvalidAbandonedReason.into());
            }
        }
        Ok(())
    }
}
