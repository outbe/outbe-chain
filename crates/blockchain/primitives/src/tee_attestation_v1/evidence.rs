use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DcapCollateralKind {
    PckCertificateChain = 0x01,
    PckCrl = 0x02,
    PckCrlIssuerChain = 0x03,
    RootCaCrl = 0x04,
    TcbInfo = 0x05,
    TcbInfoIssuerChain = 0x06,
    QeIdentity = 0x07,
    QeIdentityIssuerChain = 0x08,
}

impl TryFrom<u8> for DcapCollateralKind {
    type Error = CodecError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::PckCertificateChain),
            0x02 => Ok(Self::PckCrl),
            0x03 => Ok(Self::PckCrlIssuerChain),
            0x04 => Ok(Self::RootCaCrl),
            0x05 => Ok(Self::TcbInfo),
            0x06 => Ok(Self::TcbInfoIssuerChain),
            0x07 => Ok(Self::QeIdentity),
            0x08 => Ok(Self::QeIdentityIssuerChain),
            value => Err(CodecError::UnknownDiscriminant {
                field: "DCAP collateral component",
                value,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DcapCollateralComponentV1 {
    pub kind: DcapCollateralKind,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DcapEvidenceV1 {
    pub intent: RegistrationIntentV1,
    pub quote: Vec<u8>,
    pub components: Vec<DcapCollateralComponentV1>,
    pub transition_key_ready_proof: Option<TransitionKeyReadyProofV1>,
}

impl DcapEvidenceV1 {
    pub(super) fn encode_payload(&self) -> Result<Vec<u8>, CodecError> {
        if self.intent.attestation_mode != AttestationMode::DcapRequired {
            return Err(CodecError::NonCanonical(
                "DCAP evidence intent mode mismatch",
            ));
        }
        enforce_limit("SGX quote", MAX_QUOTE_BYTES, self.quote.len())?;
        if self.quote.is_empty() {
            return Err(CodecError::NonCanonical("empty SGX quote"));
        }
        self.validate_components()?;
        self.validate_transition_proof_presence()?;

        let intent = self.intent.encode_canonical()?;
        let mut payload_len = checked_add_usize(1, 4)?;
        payload_len = checked_add_usize(payload_len, intent.len())?;
        payload_len = checked_add_usize(payload_len, 4)?;
        payload_len = checked_add_usize(payload_len, self.quote.len())?;
        payload_len = checked_add_usize(payload_len, 2)?;
        for component in &self.components {
            payload_len = checked_add_usize(payload_len, 5)?;
            payload_len = checked_add_usize(payload_len, component.bytes.len())?;
        }
        payload_len = checked_add_usize(payload_len, 1)?;
        let transition_proof = self
            .transition_key_ready_proof
            .as_ref()
            .map(TransitionKeyReadyProofV1::encode_canonical)
            .transpose()?;
        if let Some(proof) = &transition_proof {
            payload_len = checked_add_usize(payload_len, 2)?;
            payload_len = checked_add_usize(payload_len, proof.len())?;
        }
        let complete_len = checked_add_usize(6, payload_len)?;
        enforce_limit(
            "attestation evidence",
            MAX_ATTESTATION_EVIDENCE_BYTES,
            complete_len,
        )?;

        let mut out = Vec::with_capacity(payload_len);
        out.push(PROTOCOL_VERSION_V1);
        put_len_u32(&mut out, intent.len())?;
        out.extend_from_slice(&intent);
        put_len_u32(&mut out, self.quote.len())?;
        out.extend_from_slice(&self.quote);
        put_u16(&mut out, 8);
        for component in &self.components {
            out.push(component.kind as u8);
            put_len_u32(&mut out, component.bytes.len())?;
            out.extend_from_slice(&component.bytes);
        }
        match transition_proof {
            None => out.push(0),
            Some(proof) => {
                out.push(1);
                put_u16(
                    &mut out,
                    u16::try_from(proof.len()).map_err(|_| CodecError::ArithmeticOverflow)?,
                );
                out.extend_from_slice(&proof);
            }
        }
        Ok(out)
    }

    pub(super) fn decode_payload(input: &[u8]) -> Result<Self, CodecError> {
        enforce_limit(
            "attestation evidence payload",
            MAX_ATTESTATION_EVIDENCE_BYTES,
            input.len(),
        )?;
        let mut decoder = Decoder::new(input);
        decoder.version("DcapEvidenceV1")?;
        let intent_len =
            decoder.declared_len("registration intent", MAX_EVIDENCE_CALL_FRAMING_BYTES)?;
        let intent = RegistrationIntentV1::decode_canonical(decoder.take(intent_len)?)?;
        if intent.attestation_mode != AttestationMode::DcapRequired {
            return Err(CodecError::NonCanonical(
                "DCAP evidence intent mode mismatch",
            ));
        }
        let quote_len = decoder.declared_len("SGX quote", MAX_QUOTE_BYTES)?;
        if quote_len == 0 {
            return Err(CodecError::NonCanonical("empty SGX quote"));
        }
        let quote = decoder.take(quote_len)?.to_vec();
        let components = Self::decode_components(&mut decoder)?;
        let transition_key_ready_proof = match decoder.u8()? {
            0 => None,
            1 => {
                let proof_len = usize::from(decoder.u16()?);
                if proof_len != TransitionKeyReadyProofV1::CANONICAL_LEN {
                    return Err(CodecError::NonCanonical(
                        "transition key-ready proof length",
                    ));
                }
                Some(TransitionKeyReadyProofV1::decode_canonical(
                    decoder.take(proof_len)?,
                )?)
            }
            _ => {
                return Err(CodecError::NonCanonical(
                    "transition key-ready proof presence flag",
                ))
            }
        };
        decoder.finish()?;
        let value = Self {
            intent,
            quote,
            components,
            transition_key_ready_proof,
        };
        value.validate_transition_proof_presence()?;
        Ok(value)
    }

    pub(super) fn validate_components(&self) -> Result<(), CodecError> {
        if self.components.len() != 8 {
            return Err(CodecError::NonCanonical(
                "DCAP component count must be exactly eight",
            ));
        }
        for (index, component) in self.components.iter().enumerate() {
            enforce_limit(
                "DCAP collateral component",
                MAX_COLLATERAL_COMPONENT_BYTES,
                component.bytes.len(),
            )?;
            if component.bytes.is_empty() {
                return Err(CodecError::NonCanonical("empty DCAP collateral component"));
            }
            if component.kind as u8 != (index + 1) as u8 {
                return Err(CodecError::NonCanonical(
                    "DCAP component kinds must be exactly 0x01..=0x08",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_transition_proof_presence(&self) -> Result<(), CodecError> {
        let is_transition =
            self.intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement;
        if is_transition != self.transition_key_ready_proof.is_some() {
            return Err(CodecError::NonCanonical(
                "transition key-ready proof presence does not match operation",
            ));
        }
        Ok(())
    }

    fn decode_components(
        decoder: &mut Decoder<'_>,
    ) -> Result<Vec<DcapCollateralComponentV1>, CodecError> {
        let component_count = decoder.u16()?;
        if component_count != 8 {
            return Err(CodecError::NonCanonical(
                "DCAP component count must be exactly eight",
            ));
        }
        let mut components = Vec::with_capacity(8);
        for expected_kind in 1u8..=8 {
            let kind = DcapCollateralKind::try_from(decoder.u8()?)?;
            if kind as u8 != expected_kind {
                return Err(CodecError::NonCanonical(
                    "DCAP component kinds must be exactly 0x01..=0x08",
                ));
            }
            let component_len = decoder
                .declared_len("DCAP collateral component", MAX_COLLATERAL_COMPONENT_BYTES)?;
            if component_len == 0 {
                return Err(CodecError::NonCanonical("empty DCAP collateral component"));
            }
            components.push(DcapCollateralComponentV1 {
                kind,
                bytes: decoder.take(component_len)?.to_vec(),
            });
        }
        Ok(components)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GramineDirectEvidenceV1 {
    pub intent: RegistrationIntentV1,
    pub dev_attestation_public: [u8; 32],
    pub dev_signature: [u8; 64],
    /// Payload v2 only. The v1 registration/renewal encoding remains byte-identical.
    pub transition_key_ready_proof: Option<TransitionKeyReadyProofV1>,
}

impl GramineDirectEvidenceV1 {
    pub(super) fn encode_payload(&self) -> Result<Vec<u8>, CodecError> {
        if self.intent.attestation_mode != AttestationMode::GramineDirectDev {
            return Err(CodecError::NonCanonical(
                "Gramine evidence intent mode mismatch",
            ));
        }
        if self.dev_attestation_public == [0; 32] {
            return Err(CodecError::NonCanonical(
                "zero Gramine development attestation key",
            ));
        }
        let intent = self.intent.encode_canonical()?;
        let mut out = Vec::new();
        if self.transition_key_ready_proof.is_some()
            != (self.intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement)
        {
            return Err(CodecError::NonCanonical(
                "DirectDev transition proof presence",
            ));
        }
        out.push(if self.transition_key_ready_proof.is_some() {
            2
        } else {
            PROTOCOL_VERSION_V1
        });
        put_len_u32(&mut out, intent.len())?;
        out.extend_from_slice(&intent);
        out.extend_from_slice(&self.dev_attestation_public);
        out.extend_from_slice(&self.dev_signature);
        if let Some(proof) = &self.transition_key_ready_proof {
            out.extend_from_slice(&proof.encode_canonical()?);
        }
        Ok(out)
    }

    pub(super) fn decode_payload(input: &[u8]) -> Result<Self, CodecError> {
        let mut decoder = Decoder::new(input);
        let version = decoder.u8()?;
        if version != 1 && version != 2 {
            return Err(CodecError::NonCanonical("DirectDev evidence version"));
        }
        let intent_len =
            decoder.declared_len("registration intent", MAX_EVIDENCE_CALL_FRAMING_BYTES)?;
        let value = Self {
            intent: RegistrationIntentV1::decode_canonical(decoder.take(intent_len)?)?,
            dev_attestation_public: decoder.array()?,
            dev_signature: decoder.array()?,
            transition_key_ready_proof: if version == 2 {
                Some(TransitionKeyReadyProofV1::decode_canonical(
                    decoder.take(TransitionKeyReadyProofV1::CANONICAL_LEN)?,
                )?)
            } else {
                None
            },
        };
        decoder.finish()?;
        if (version == 2)
            != (value.intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement)
        {
            return Err(CodecError::NonCanonical(
                "DirectDev transition evidence version",
            ));
        }
        if value.intent.attestation_mode != AttestationMode::GramineDirectDev {
            return Err(CodecError::NonCanonical(
                "Gramine evidence intent mode mismatch",
            ));
        }
        if value.dev_attestation_public == [0; 32] {
            return Err(CodecError::NonCanonical(
                "zero Gramine development attestation key",
            ));
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttestationEvidenceV1 {
    Dcap(DcapEvidenceV1),
    GramineDirectDev(GramineDirectEvidenceV1),
}

impl AttestationEvidenceV1 {
    pub fn intent(&self) -> &RegistrationIntentV1 {
        match self {
            Self::Dcap(v) => &v.intent,
            Self::GramineDirectDev(v) => &v.intent,
        }
    }
    pub fn transition_key_ready_proof(&self) -> Option<&TransitionKeyReadyProofV1> {
        match self {
            Self::Dcap(v) => v.transition_key_ready_proof.as_ref(),
            Self::GramineDirectDev(v) => v.transition_key_ready_proof.as_ref(),
        }
    }
    pub fn encode_canonical(&self) -> Result<Vec<u8>, CodecError> {
        let (mode, payload) = match self {
            Self::Dcap(value) => (AttestationMode::DcapRequired, value.encode_payload()?),
            Self::GramineDirectDev(value) => {
                (AttestationMode::GramineDirectDev, value.encode_payload()?)
            }
        };
        let mut out = Vec::new();
        out.push(PROTOCOL_VERSION_V1);
        out.push(mode as u8);
        put_len_u32(&mut out, payload.len())?;
        out.extend_from_slice(&payload);
        enforce_limit(
            "attestation evidence",
            MAX_ATTESTATION_EVIDENCE_BYTES,
            out.len(),
        )?;
        Ok(out)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, CodecError> {
        enforce_limit(
            "attestation evidence",
            MAX_ATTESTATION_EVIDENCE_BYTES,
            input.len(),
        )?;
        let mut decoder = Decoder::new(input);
        decoder.version("AttestationEvidenceV1")?;
        let mode = AttestationMode::decode(decoder.u8()?)?;
        let payload_len = decoder.declared_len(
            "attestation evidence payload",
            MAX_ATTESTATION_EVIDENCE_BYTES,
        )?;
        let payload = decoder.take(payload_len)?;
        let value = match mode {
            AttestationMode::DcapRequired => Self::Dcap(DcapEvidenceV1::decode_payload(payload)?),
            AttestationMode::GramineDirectDev => {
                Self::GramineDirectDev(GramineDirectEvidenceV1::decode_payload(payload)?)
            }
        };
        decoder.finish()?;
        Ok(value)
    }

    pub fn evidence_hash(&self) -> Result<B256, CodecError> {
        Ok(domain_hash(
            ATTESTATION_EVIDENCE_DOMAIN_V1,
            &self.encode_canonical()?,
        ))
    }

    pub const fn mode(&self) -> AttestationMode {
        match self {
            Self::Dcap(_) => AttestationMode::DcapRequired,
            Self::GramineDirectDev(_) => AttestationMode::GramineDirectDev,
        }
    }
}
