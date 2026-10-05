use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AttestationOperationV1 {
    RegisterEnclave = 0x01,
    RenewEnclave = 0x02,
    TransitionEnclaveMeasurement = 0x03,
    ReplaceEnclaveBinding = 0x04,
    PrepareEnclaveUpgrade = 0x05,
}

impl AttestationOperationV1 {
    pub const fn is_measurement_upgrade(self) -> bool {
        matches!(
            self,
            Self::TransitionEnclaveMeasurement | Self::PrepareEnclaveUpgrade
        )
    }

    pub(super) fn decode(value: u8) -> Result<Self, CodecError> {
        match value {
            0x01 => Ok(Self::RegisterEnclave),
            0x02 => Ok(Self::RenewEnclave),
            0x03 => Ok(Self::TransitionEnclaveMeasurement),
            0x04 => Ok(Self::ReplaceEnclaveBinding),
            0x05 => Ok(Self::PrepareEnclaveUpgrade),
            value => Err(CodecError::UnknownDiscriminant {
                field: "attestation operation",
                value,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationIntentV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub operation: AttestationOperationV1,
    pub attestation_mode: AttestationMode,
    pub policy_hash: B256,
    pub node_id: NodeIdV1,
    pub enclave_id: B256,
    pub binding_id: B256,
    pub binding_version: u64,
    pub registration_version: u64,
    pub renewal_nonce: u64,
    pub transition_nonce: u64,
    pub requested_valid_until: u64,
    pub recipient_x25519: [u8; 32],
    pub attestation_ed25519: [u8; 32],
    pub noise_responder_x25519: [u8; 32],
    pub node_host_authorization_hash: B256,
}

impl RegistrationIntentV1 {
    pub const fn network_binding(&self) -> NetworkBindingV1 {
        NetworkBindingV1 {
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            attestation_mode: self.attestation_mode,
        }
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(352);
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        out.push(self.operation as u8);
        out.push(self.attestation_mode as u8);
        out.extend_from_slice(self.policy_hash.as_slice());
        self.node_id.encode_into(&mut out);
        out.extend_from_slice(self.enclave_id.as_slice());
        out.extend_from_slice(self.binding_id.as_slice());
        put_u64(&mut out, self.binding_version);
        put_u64(&mut out, self.registration_version);
        put_u64(&mut out, self.renewal_nonce);
        put_u64(&mut out, self.transition_nonce);
        put_u64(&mut out, self.requested_valid_until);
        out.extend_from_slice(&self.recipient_x25519);
        out.extend_from_slice(&self.attestation_ed25519);
        out.extend_from_slice(&self.noise_responder_x25519);
        out.extend_from_slice(self.node_host_authorization_hash.as_slice());
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        decoder.version("RegistrationIntentV1")?;
        let value = Self {
            chain_id: decoder.array()?,
            genesis_hash: B256::from(decoder.array::<32>()?),
            operation: AttestationOperationV1::decode(decoder.u8()?)?,
            attestation_mode: AttestationMode::decode(decoder.u8()?)?,
            policy_hash: B256::from(decoder.array::<32>()?),
            node_id: NodeIdV1::decode_from(&mut decoder)?,
            enclave_id: B256::from(decoder.array::<32>()?),
            binding_id: B256::from(decoder.array::<32>()?),
            binding_version: decoder.u64()?,
            registration_version: decoder.u64()?,
            renewal_nonce: decoder.u64()?,
            transition_nonce: decoder.u64()?,
            requested_valid_until: decoder.u64()?,
            recipient_x25519: decoder.array()?,
            attestation_ed25519: decoder.array()?,
            noise_responder_x25519: decoder.array()?,
            node_host_authorization_hash: B256::from(decoder.array::<32>()?),
        };
        decoder.finish()?;
        value.validate()?;
        Ok(value)
    }

    pub fn intent_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            REGISTRATION_INTENT_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    /// Candidate identity excludes lease/renewal counters: a live old binding may
    /// renew while the candidate receives its key. The final transition still
    /// validates fresh counters independently.
    pub fn upgrade_target_hash(&self) -> Result<B256, CodecError> {
        let mut target = self.clone();
        target.operation = AttestationOperationV1::PrepareEnclaveUpgrade;
        target.registration_version = 0;
        target.renewal_nonce = 0;
        target.transition_nonce = 0;
        target.requested_valid_until = 1;
        Ok(domain_hash(
            b"outbe/tee/upgrade-target/v1",
            &target.encode_canonical()?,
        ))
    }

    /// Verify the NodeHost proof of possession over this exact registration
    /// authorization. TeeRegistry separately authenticates the EVM caller
    /// against the canonical address-to-NodeHost association.
    pub fn verify_node_signature(&self, signature: &[u8; 65]) -> bool {
        let Ok(hash) = self.intent_hash() else {
            return false;
        };
        crate::tee_signatures::recover_signer_public_key(&hash, signature)
            .map(|recovered| recovered == self.node_id.reth_p2p_public)
            .unwrap_or(false)
    }

    /// Verify that the quote-bound enclave attestation key signed the same
    /// canonical authorization as the node identity.
    pub fn verify_enclave_signature(&self, signature: &[u8; 64]) -> bool {
        let Ok(hash) = self.intent_hash() else {
            return false;
        };
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, self.attestation_ed25519)
            .verify(hash.as_slice(), signature)
            .is_ok()
    }

    /// Derive the stable enclave identity from the three persistent public
    /// keys carried by this intent. Registry code uses this instead of trusting
    /// a caller-selected `enclave_id` as the one-to-one reverse-map key.
    pub fn derived_enclave_id(&self) -> Result<B256, CodecError> {
        self.validate()?;
        let mut keys = [0u8; 96];
        keys[..32].copy_from_slice(&self.recipient_x25519);
        keys[32..64].copy_from_slice(&self.attestation_ed25519);
        keys[64..].copy_from_slice(&self.noise_responder_x25519);
        Ok(domain_hash(ENCLAVE_ID_DOMAIN_V1, &keys))
    }

    pub fn report_policy_hash(&self) -> B256 {
        let mut canonical = [0u8; 96];
        canonical[..32].copy_from_slice(self.genesis_hash.as_slice());
        canonical[32..64].copy_from_slice(self.policy_hash.as_slice());
        canonical[64..].copy_from_slice(self.node_host_authorization_hash.as_slice());
        domain_hash(REPORT_POLICY_DOMAIN_V1, &canonical)
    }

    pub fn report_data(&self) -> Result<[u8; 64], CodecError> {
        let mut report_data = [0u8; 64];
        report_data[..32].copy_from_slice(self.intent_hash()?.as_slice());
        report_data[32..].copy_from_slice(self.report_policy_hash().as_slice());
        Ok(report_data)
    }

    pub fn validate_chain_identity(
        &self,
        chain_id: [u8; 32],
        genesis_hash: B256,
    ) -> Result<(), CodecError> {
        if self.chain_id != chain_id || self.genesis_hash != genesis_hash {
            return Err(CodecError::ChainIdentityMismatch);
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        self.node_id.validate()?;
        if [
            self.genesis_hash,
            self.policy_hash,
            self.enclave_id,
            self.binding_id,
            self.node_host_authorization_hash,
        ]
        .iter()
        .any(B256::is_zero)
        {
            return Err(CodecError::NonCanonical(
                "registration intent contains a zero commitment",
            ));
        }
        if self.recipient_x25519 == [0; 32]
            || self.attestation_ed25519 == [0; 32]
            || self.noise_responder_x25519 == [0; 32]
        {
            return Err(CodecError::NonCanonical(
                "registration intent contains a zero public key",
            ));
        }
        if self.binding_version == 0 || self.requested_valid_until == 0 {
            return Err(CodecError::NonCanonical(
                "registration intent contains a zero version or lease",
            ));
        }
        Ok(())
    }
}
