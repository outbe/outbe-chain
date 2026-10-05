use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeIdV1 {
    pub reth_p2p_public: [u8; 33],
}

impl NodeIdV1 {
    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(38);
        self.encode_into(&mut out);
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        let value = Self::decode_from(&mut decoder)?;
        decoder.finish()?;
        Ok(value)
    }

    pub fn node_id_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(NODE_ID_DOMAIN_V1, &self.encode_canonical()?))
    }

    pub(super) fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(PROTOCOL_VERSION_V1);
        put_u32(out, 33);
        out.extend_from_slice(&self.reth_p2p_public);
    }

    pub(super) fn decode_from(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        decoder.version("NodeIdV1")?;
        let payload_len = decoder.declared_len("node id payload", 33)?;
        if payload_len != 33 {
            return Err(CodecError::NonCanonical("node id length"));
        }
        let value = Self {
            reth_p2p_public: decoder.array()?,
        };
        value.validate()?;
        Ok(value)
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if matches!(self.reth_p2p_public[0], 0x02 | 0x03)
            && self.reth_p2p_public[1..] != [0; 32]
            && k256::PublicKey::from_sec1_bytes(&self.reth_p2p_public).is_ok()
        {
            Ok(())
        } else {
            Err(CodecError::NonCanonical(
                "node id is not canonical compressed secp256k1",
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatorNodeBindingV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub validator: [u8; 20],
    pub node_id_hash: B256,
}

impl ValidatorNodeBindingV1 {
    pub const CANONICAL_LEN: usize = 1 + 32 + 32 + 20 + 32;

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(Self::CANONICAL_LEN);
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        out.extend_from_slice(&self.validator);
        out.extend_from_slice(self.node_id_hash.as_slice());
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        if input.len() != Self::CANONICAL_LEN {
            return Err(CodecError::NonCanonical(
                "validator NodeHost binding length",
            ));
        }
        let mut decoder = Decoder::new(input);
        decoder.version("ValidatorNodeBindingV1")?;
        let value = Self {
            chain_id: decoder.array()?,
            genesis_hash: B256::from(decoder.array::<32>()?),
            validator: decoder.array()?,
            node_id_hash: B256::from(decoder.array::<32>()?),
        };
        decoder.finish()?;
        value.validate()?;
        Ok(value)
    }

    pub fn binding_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            VALIDATOR_NODE_BINDING_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
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

    pub fn verify_validator_signature(&self, signature: &[u8; 65]) -> bool {
        let Ok(hash) = self.binding_hash() else {
            return false;
        };
        crate::tee_signatures::recover_signer(&hash, signature)
            .map(|recovered| recovered.as_slice() == self.validator)
            .unwrap_or(false)
    }

    pub fn verify_node_signature(&self, signature: &[u8; 65]) -> bool {
        let Ok(hash) = self.binding_hash() else {
            return false;
        };
        crate::tee_signatures::recover_signer_public_key(&hash, signature)
            .ok()
            .and_then(|reth_p2p_public| NodeIdV1 { reth_p2p_public }.node_id_hash().ok())
            == Some(self.node_id_hash)
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if self.chain_id == [0; 32]
            || self.genesis_hash.is_zero()
            || self.validator == [0; 20]
            || self.node_id_hash.is_zero()
        {
            return Err(CodecError::NonCanonical(
                "validator NodeHost binding contains a zero identity or commitment",
            ));
        }
        Ok(())
    }
}

/// Bounded canonical preimage of the stable `NodeHost` authorization committed
/// by every registration. Remote peers disclose this public witness so the
/// target can recover the exact Noise IK initiator static from finalized state
/// without accepting a full initialization manifest or a host assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeHostAuthorizationWitnessV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub attestation_mode: AttestationMode,
    pub node_id: NodeIdV1,
    pub node_host_noise_x25519: [u8; 32],
}

impl NodeHostAuthorizationWitnessV1 {
    pub fn from_manifest(manifest: &EnclaveInitializationManifestV1) -> Result<Self, CodecError> {
        manifest.validate()?;
        Ok(Self {
            chain_id: manifest.chain_id,
            genesis_hash: manifest.genesis_hash,
            attestation_mode: manifest.attestation_mode,
            node_id: manifest.node_id.clone(),
            node_host_noise_x25519: manifest.node_host_noise_x25519,
        })
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(MAX_NODE_HOST_AUTHORIZATION_WITNESS_BYTES);
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        out.push(self.attestation_mode as u8);
        self.node_id.encode_into(&mut out);
        out.extend_from_slice(&self.node_host_noise_x25519);
        enforce_limit(
            "NodeHost authorization witness",
            MAX_NODE_HOST_AUTHORIZATION_WITNESS_BYTES,
            out.len(),
        )?;
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        enforce_limit(
            "NodeHost authorization witness",
            MAX_NODE_HOST_AUTHORIZATION_WITNESS_BYTES,
            input.len(),
        )?;
        let mut decoder = Decoder::new(input);
        decoder.version("NodeHostAuthorizationWitnessV1")?;
        let value = Self {
            chain_id: decoder.array()?,
            genesis_hash: B256::from(decoder.array::<32>()?),
            attestation_mode: AttestationMode::decode(decoder.u8()?)?,
            node_id: NodeIdV1::decode_from(&mut decoder)?,
            node_host_noise_x25519: decoder.array()?,
        };
        decoder.finish()?;
        value.validate()?;
        Ok(value)
    }

    pub fn authorization_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            NODE_HOST_AUTHORIZATION_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        self.node_id.validate()?;
        if self.chain_id == [0; 32]
            || self.genesis_hash.is_zero()
            || self.node_host_noise_x25519 == [0; 32]
        {
            return Err(CodecError::NonCanonical(
                "NodeHost authorization witness contains a zero identity or commitment",
            ));
        }
        Ok(())
    }
}

/// The single exact initialization manifest an enclave accepts before sealing
/// its V1 identity. The node signature is carried separately from this canonical
/// payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnclaveInitializationManifestV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub attestation_mode: AttestationMode,
    pub node_id: NodeIdV1,
    pub initialization_challenge: [u8; 32],
    pub node_host_noise_x25519: [u8; 32],
    pub recipient_x25519: [u8; 32],
    pub attestation_ed25519: [u8; 32],
    pub noise_responder_x25519: [u8; 32],
}

impl EnclaveInitializationManifestV1 {
    pub const fn network_binding(&self) -> NetworkBindingV1 {
        NetworkBindingV1 {
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            attestation_mode: self.attestation_mode,
        }
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(300);
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        out.push(self.attestation_mode as u8);
        self.node_id.encode_into(&mut out);
        out.extend_from_slice(&self.initialization_challenge);
        out.extend_from_slice(&self.node_host_noise_x25519);
        out.extend_from_slice(&self.recipient_x25519);
        out.extend_from_slice(&self.attestation_ed25519);
        out.extend_from_slice(&self.noise_responder_x25519);
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        decoder.version("EnclaveInitializationManifestV1")?;
        let value = Self {
            chain_id: decoder.array()?,
            genesis_hash: B256::from(decoder.array::<32>()?),
            attestation_mode: AttestationMode::decode(decoder.u8()?)?,
            node_id: NodeIdV1::decode_from(&mut decoder)?,
            initialization_challenge: decoder.array()?,
            node_host_noise_x25519: decoder.array()?,
            recipient_x25519: decoder.array()?,
            attestation_ed25519: decoder.array()?,
            noise_responder_x25519: decoder.array()?,
        };
        decoder.finish()?;
        value.validate()?;
        Ok(value)
    }

    pub fn authorization_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            INITIALIZATION_MANIFEST_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    /// Stable authority shared by successive enclave initializations for one
    /// node. Fresh initialization challenges and enclave keys are deliberately
    /// excluded; changing the persistent NodeHost key or node identity changes
    /// this commitment.
    pub fn node_host_authorization_hash(&self) -> Result<B256, CodecError> {
        NodeHostAuthorizationWitnessV1::from_manifest(self)?.authorization_hash()
    }

    /// Stable enclave identity derived from the three persistent public keys
    /// committed by every registration and renewal intent.
    pub fn enclave_id(&self) -> Result<B256, CodecError> {
        self.validate()?;
        let mut keys = [0u8; 96];
        keys[..32].copy_from_slice(&self.recipient_x25519);
        keys[32..64].copy_from_slice(&self.attestation_ed25519);
        keys[64..].copy_from_slice(&self.noise_responder_x25519);
        Ok(domain_hash(ENCLAVE_ID_DOMAIN_V1, &keys))
    }

    /// Verify the node proof of possession over the exact canonical manifest.
    /// Validators authorize with their EVM key; full nodes authorize with the
    /// compressed secp256k1 key already used as their Reth P2P identity.
    pub fn verify_node_signature(&self, signature: &[u8; 65]) -> bool {
        let Ok(hash) = self.authorization_hash() else {
            return false;
        };
        crate::tee_signatures::recover_signer_public_key(&hash, signature)
            .map(|recovered| recovered == self.node_id.reth_p2p_public)
            .unwrap_or(false)
    }

    /// Ensure a requested quote is for this exact initialized identity. Dynamic
    /// operation/version/nonce/lease/policy fields remain part of the intent and
    /// may change; node, chain, profile and persistent key authority may not.
    pub fn validate_intent_binding(&self, intent: &RegistrationIntentV1) -> Result<(), CodecError> {
        self.validate()?;
        intent.validate()?;
        let same_network_and_node =
            intent.network_binding() == self.network_binding() && intent.node_id == self.node_id;
        if !same_network_and_node
            || intent.enclave_id != self.enclave_id()?
            || !self.intent_matches_persistent_keys(intent)
            || intent.node_host_authorization_hash != self.node_host_authorization_hash()?
        {
            return Err(CodecError::NonCanonical(
                "registration intent does not match initialized enclave",
            ));
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        self.node_id.validate()?;
        if self.chain_id == [0; 32]
            || self.genesis_hash.is_zero()
            || [
                self.initialization_challenge,
                self.node_host_noise_x25519,
                self.recipient_x25519,
                self.attestation_ed25519,
                self.noise_responder_x25519,
            ]
            .contains(&[0; 32])
        {
            return Err(CodecError::NonCanonical(
                "initialization manifest contains a zero identity or commitment",
            ));
        }
        Ok(())
    }

    fn intent_matches_persistent_keys(&self, intent: &RegistrationIntentV1) -> bool {
        intent.recipient_x25519 == self.recipient_x25519
            && intent.attestation_ed25519 == self.attestation_ed25519
            && intent.noise_responder_x25519 == self.noise_responder_x25519
    }
}
