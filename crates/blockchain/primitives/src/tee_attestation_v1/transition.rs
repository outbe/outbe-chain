use super::*;

/// Candidate-enclave proof that a measurement successor already holds the
/// chain's permanent offer key before Registry binding mutation.
///
/// The candidate computes its own initialized-manifest hash and signs the
/// exact transition context with the quote-bound Ed25519 key. Registry can
/// independently verify every field except the local manifest journal link;
/// NodeHost additionally compares `candidate_manifest_hash` with its durable
/// candidate before persisting or promoting the submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionKeyReadyProofV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub transition_intent_hash: B256,
    pub candidate_manifest_hash: B256,
    pub transition_nonce: u64,
    pub resident_offer_public: [u8; 32],
    pub candidate_attestation_signature: [u8; 64],
}

impl TransitionKeyReadyProofV1 {
    pub const CANONICAL_LEN: usize = 1 + 32 + 32 + 32 + 32 + 8 + 32 + 64;

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate_shape()?;
        let mut out = Vec::with_capacity(Self::CANONICAL_LEN);
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        out.extend_from_slice(self.transition_intent_hash.as_slice());
        out.extend_from_slice(self.candidate_manifest_hash.as_slice());
        put_u64(&mut out, self.transition_nonce);
        out.extend_from_slice(&self.resident_offer_public);
        out.extend_from_slice(&self.candidate_attestation_signature);
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        if input.len() != Self::CANONICAL_LEN {
            return Err(CodecError::NonCanonical(
                "transition key-ready proof length",
            ));
        }
        let mut decoder = Decoder::new(input);
        decoder.version("TransitionKeyReadyProofV1")?;
        let value = Self {
            chain_id: decoder.array()?,
            genesis_hash: B256::from(decoder.array::<32>()?),
            transition_intent_hash: B256::from(decoder.array::<32>()?),
            candidate_manifest_hash: B256::from(decoder.array::<32>()?),
            transition_nonce: decoder.u64()?,
            resident_offer_public: decoder.array()?,
            candidate_attestation_signature: decoder.array()?,
        };
        decoder.finish()?;
        value.validate_shape()?;
        Ok(value)
    }

    /// Hash signed by the initialized candidate enclave. The signature itself
    /// is deliberately excluded from the preimage.
    pub fn signing_hash(&self) -> Result<B256, CodecError> {
        self.validate_shape()?;
        let mut canonical = Vec::with_capacity(Self::CANONICAL_LEN - 64);
        canonical.push(PROTOCOL_VERSION_V1);
        canonical.extend_from_slice(&self.chain_id);
        canonical.extend_from_slice(self.genesis_hash.as_slice());
        canonical.extend_from_slice(self.transition_intent_hash.as_slice());
        canonical.extend_from_slice(self.candidate_manifest_hash.as_slice());
        put_u64(&mut canonical, self.transition_nonce);
        canonical.extend_from_slice(&self.resident_offer_public);
        Ok(domain_hash(
            TRANSITION_KEY_READY_PROOF_DOMAIN_V1,
            &canonical,
        ))
    }

    /// Verify the proof against the exact transition intent and Registry offer
    /// public key. No host-supplied candidate or chain value is trusted.
    pub fn verify_for_transition(
        &self,
        intent: &RegistrationIntentV1,
        expected_offer_public: [u8; 32],
    ) -> Result<(), CodecError> {
        self.validate_shape()?;
        let mode_and_network_match = intent.operation
            == AttestationOperationV1::TransitionEnclaveMeasurement
            && !self.network_binding_differs_from(intent);
        if !mode_and_network_match
            || self.transition_intent_hash != intent.intent_hash()?
            || self.transition_nonce != intent.transition_nonce
            || self.resident_offer_public != expected_offer_public
        {
            return Err(CodecError::NonCanonical(
                "transition key-ready proof binding mismatch",
            ));
        }
        ring::signature::UnparsedPublicKey::new(
            &ring::signature::ED25519,
            intent.attestation_ed25519,
        )
        .verify(
            self.signing_hash()?.as_slice(),
            &self.candidate_attestation_signature,
        )
        .map_err(|_| CodecError::NonCanonical("transition key-ready proof signature"))
    }

    pub(super) fn validate_shape(&self) -> Result<(), CodecError> {
        if self.chain_id == [0; 32]
            || [
                self.genesis_hash,
                self.transition_intent_hash,
                self.candidate_manifest_hash,
            ]
            .iter()
            .any(B256::is_zero)
            || self.transition_nonce == 0
            || self.resident_offer_public == [0; 32]
        {
            return Err(CodecError::NonCanonical(
                "transition key-ready proof contains a zero commitment",
            ));
        }
        Ok(())
    }

    fn network_binding_differs_from(&self, intent: &RegistrationIntentV1) -> bool {
        self.chain_id != intent.chain_id || self.genesis_hash != intent.genesis_hash
    }
}
