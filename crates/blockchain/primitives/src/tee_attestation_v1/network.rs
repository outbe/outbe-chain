use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AttestationMode {
    DcapRequired = 0x01,
    GramineDirectDev = 0x02,
}

impl AttestationMode {
    pub(crate) fn decode(value: u8) -> Result<Self, CodecError> {
        match value {
            0x01 => Ok(Self::DcapRequired),
            0x02 => Ok(Self::GramineDirectDev),
            value => Err(CodecError::UnknownDiscriminant {
                field: "attestation mode",
                value,
            }),
        }
    }
}

/// Immutable public identity of one TEE network. The binding is selected by the
/// ChainSpec and sealed by the enclave; it is never inferred from a host runtime
/// fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkBindingV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub attestation_mode: AttestationMode,
}

impl NetworkBindingV1 {
    pub const CANONICAL_LEN: usize = 1 + 32 + 32 + 1;

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = Vec::with_capacity(Self::CANONICAL_LEN);
        out.push(PROTOCOL_VERSION_V1);
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(self.genesis_hash.as_slice());
        out.push(self.attestation_mode as u8);
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        decoder.version("NetworkBindingV1")?;
        let value = Self {
            chain_id: decoder.array()?,
            genesis_hash: B256::from(decoder.array::<32>()?),
            attestation_mode: AttestationMode::decode(decoder.u8()?)?,
        };
        decoder.finish()?;
        value.validate()?;
        Ok(value)
    }

    pub fn binding_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            NETWORK_BINDING_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if self.chain_id == [0; 32] || self.genesis_hash.is_zero() {
            return Err(CodecError::NonCanonical(
                "network binding contains a zero chain identity",
            ));
        }
        Ok(())
    }
}

/// Release-measured authority for one SGX network.
///
/// The ordinary [`NetworkBindingV1`] is carried through node/enclave protocol
/// messages. A modified NodeHost can choose those bytes, so they are not by
/// themselves a trust root. SGX release bundles additionally measure this
/// descriptor as a Gramine trusted file. The enclave accepts an initialization
/// manifest only when its network binding equals this descriptor exactly.
///
/// `genesis_consensus_keys` are derived from that same final ChainSpec. The
/// sorted MinPk keys are the epoch-0 finality trust root; every later committee
/// must be authenticated by a certificate from its predecessor. The active TEE
/// policy is deliberately not measured here: it contains `MRENCLAVE`, while a
/// Gramine trusted file contributes to `MRENCLAVE`. Instead, onboarding proves
/// the active policy hash from finalized TeeRegistry state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedNetworkDescriptorV1 {
    pub network_binding: NetworkBindingV1,
    pub genesis_consensus_keys: Vec<[u8; 48]>,
}

impl TrustedNetworkDescriptorV1 {
    pub const FIXED_CANONICAL_LEN: usize = NetworkBindingV1::CANONICAL_LEN + 2;
    pub const MAX_GENESIS_CONSENSUS_KEYS: usize = 256;

    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        self.validate()?;
        let mut out = self.network_binding.encode_canonical()?;
        out.extend_from_slice(&(self.genesis_consensus_keys.len() as u16).to_be_bytes());
        for key in &self.genesis_consensus_keys {
            out.extend_from_slice(key);
        }
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        if input.len() < Self::FIXED_CANONICAL_LEN {
            return Err(CodecError::NonCanonical(
                "trusted network descriptor length",
            ));
        }
        let mut decoder = Decoder::new(&input[NetworkBindingV1::CANONICAL_LEN..]);
        let network_binding =
            NetworkBindingV1::decode_canonical(&input[..NetworkBindingV1::CANONICAL_LEN])?;
        let key_count = usize::from(decoder.u16()?);
        if key_count == 0 || key_count > Self::MAX_GENESIS_CONSENSUS_KEYS {
            return Err(CodecError::NonCanonical(
                "trusted network descriptor genesis committee size",
            ));
        }
        let mut genesis_consensus_keys = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            genesis_consensus_keys.push(decoder.array::<48>()?);
        }
        decoder.finish()?;
        let value = Self {
            network_binding,
            genesis_consensus_keys,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn descriptor_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            TRUSTED_NETWORK_DESCRIPTOR_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        self.network_binding.validate()?;
        if self.network_binding.attestation_mode == AttestationMode::GramineDirectDev {
            let chain_id = u64::try_from(alloy_primitives::U256::from_be_bytes(
                self.network_binding.chain_id,
            ))
            .map_err(|_| CodecError::NonCanonical("descriptor chain id exceeds u64"))?;
            if !crate::tee_genesis_v1::is_attestation_mode_allowed_for_chain_id(
                chain_id,
                self.network_binding.attestation_mode,
            ) {
                return Err(CodecError::NonCanonical(
                    "descriptor attestation mode is forbidden for this network",
                ));
            }
        }
        if self.genesis_consensus_keys.is_empty()
            || self.genesis_consensus_keys.len() > Self::MAX_GENESIS_CONSENSUS_KEYS
        {
            return Err(CodecError::NonCanonical(
                "trusted network descriptor genesis committee size",
            ));
        }
        if self
            .genesis_consensus_keys
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(CodecError::NonCanonical(
                "trusted network descriptor genesis committee order",
            ));
        }
        Ok(())
    }
}

/// Hash a non-empty DKG participant set in canonical BLS-public-key order.
/// Duplicate identities are rejected before any ceremony-specific signature is
/// produced or accepted.
pub fn dkg_participant_set_hash_v1(participant_bls: &[Vec<u8>]) -> Result<B256, CodecError> {
    if participant_bls.is_empty() {
        return Err(CodecError::NonCanonical("empty DKG participant set"));
    }
    let mut ordered = participant_bls.to_vec();
    ordered.sort();
    if ordered.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CodecError::NonCanonical(
            "duplicate DKG participant identity",
        ));
    }
    let mut encoded = Vec::new();
    encoded.extend_from_slice(
        &u32::try_from(ordered.len())
            .map_err(|_| CodecError::ArithmeticOverflow)?
            .to_be_bytes(),
    );
    for public_key in ordered {
        encoded.extend_from_slice(
            &u32::try_from(public_key.len())
                .map_err(|_| CodecError::ArithmeticOverflow)?
                .to_be_bytes(),
        );
        encoded.extend_from_slice(&public_key);
    }
    Ok(domain_hash(DKG_PARTICIPANT_SET_DOMAIN_V1, &encoded))
}

/// Derive the only accepted ceremony id for a network, round and exact
/// participant set. A host cannot reuse an announcement in another network or
/// silently open a different committee under the same id.
pub fn dkg_ceremony_id_v1(
    network_binding: &NetworkBindingV1,
    round: u64,
    participant_set_hash: B256,
) -> Result<B256, CodecError> {
    if participant_set_hash.is_zero() {
        return Err(CodecError::NonCanonical("zero DKG participant set hash"));
    }
    let mut encoded = network_binding.encode_canonical()?;
    encoded.extend_from_slice(&round.to_be_bytes());
    encoded.extend_from_slice(participant_set_hash.as_slice());
    Ok(domain_hash(DKG_CEREMONY_DOMAIN_V1, &encoded))
}

/// Exact digest signed by a DKG participant when announcing its share-recipient
/// key. Every ceremony discriminator is inside the signature.
pub fn dkg_participant_announce_hash_v1(
    network_binding: &NetworkBindingV1,
    ceremony_id: B256,
    round: u64,
    participant_set_hash: B256,
    dkg_enc_pub: &[u8; 32],
) -> Result<B256, CodecError> {
    let expected_ceremony = dkg_ceremony_id_v1(network_binding, round, participant_set_hash)?;
    if ceremony_id != expected_ceremony {
        return Err(CodecError::NonCanonical("non-canonical DKG ceremony id"));
    }
    let mut encoded = network_binding.encode_canonical()?;
    encoded.extend_from_slice(ceremony_id.as_slice());
    encoded.extend_from_slice(&round.to_be_bytes());
    encoded.extend_from_slice(participant_set_hash.as_slice());
    encoded.extend_from_slice(dkg_enc_pub);
    Ok(domain_hash(DKG_PARTICIPANT_ANNOUNCE_DOMAIN_V1, &encoded))
}
