//! Canonical bootstrap byte encoding, bounded decoding and logical evidence reconstruction.
use super::*;

/// Encode a payload after canonical admission checks.
pub fn encode_canonical(payload: &TeeBootstrapV2) -> Result<Bytes, CodecError> {
    payload.validate()?;
    let mut out = payload.encode_body()?;
    put_len_u16(&mut out, payload.committee_signatures.len())?;
    for signature in &payload.committee_signatures {
        out.extend_from_slice(signature.validator.as_slice());
        out.extend_from_slice(&signature.signature);
    }
    enforce_full_calldata_cap(out.len())?;
    Ok(Bytes::from(out))
}

pub(super) fn encode_body(payload: &TeeBootstrapV2) -> Result<Vec<u8>, CodecError> {
    let policy = payload.authority.policy.encode_canonical()?;
    let mut out = Vec::with_capacity(payload.encoded_body_len()?);
    out.extend_from_slice(MAGIC);
    put_bytes_u32(&mut out, &policy)?;
    out.extend_from_slice(payload.authority.committee_snapshot_hash.as_slice());
    put_u64(&mut out, payload.authority.committee_snapshot_block);
    put_u64(&mut out, payload.authority.key_epoch);
    put_u64(&mut out, payload.authority.tribute_offer_epoch);
    out.extend_from_slice(payload.authority.dkg_transcript_hash.as_slice());
    out.extend_from_slice(payload.authority.tribute_offer_public_key.as_slice());
    put_bytes_u32(
        &mut out,
        payload.authority.tribute_offer_group_public_key.as_ref(),
    )?;

    put_len_u16(&mut out, payload.collateral_pool.len())?;
    for component in &payload.collateral_pool {
        out.push(component.kind as u8);
        put_bytes_u32(&mut out, &component.bytes)?;
    }

    put_len_u16(&mut out, payload.participants.len())?;
    for participant in &payload.participants {
        encode_participant(participant, &mut out)?;
    }
    Ok(out)
}

fn encode_participant(
    participant: &TeeBootstrapParticipantV2,
    out: &mut Vec<u8>,
) -> Result<(), CodecError> {
    put_bytes_u32(out, &participant.intent.encode_canonical()?)?;
    out.extend_from_slice(&participant.validator_binding.encode_canonical()?);
    out.extend_from_slice(&participant.validator_signature);
    out.extend_from_slice(&participant.node_binding_signature);
    out.push(participant.intent.attestation_mode as u8);
    encode_evidence(&participant.evidence, out)?;
    out.extend_from_slice(&participant.node_signature);
    out.extend_from_slice(&participant.enclave_signature);
    Ok(())
}

fn encode_evidence(
    evidence: &TeeBootstrapParticipantEvidenceV2,
    out: &mut Vec<u8>,
) -> Result<(), CodecError> {
    match evidence {
        TeeBootstrapParticipantEvidenceV2::Dcap {
            quote,
            collateral_component_indices,
        } => {
            put_bytes_u32(out, quote)?;
            for index in collateral_component_indices {
                put_u16(out, *index);
            }
        }
        TeeBootstrapParticipantEvidenceV2::GramineDirectDev {
            dev_attestation_public,
            dev_signature,
        } => {
            out.extend_from_slice(dev_attestation_public);
            out.extend_from_slice(dev_signature);
        }
    }
    Ok(())
}

/// Decode bounded canonical bytes and validate the resulting payload.
pub fn decode_canonical(input: &[u8]) -> Result<TeeBootstrapV2, CodecError> {
    enforce_full_calldata_cap(input.len())?;
    let mut decoder = Decoder { input, cursor: 0 };
    if decoder.take(4)? != MAGIC {
        return Err(CodecError::NonCanonical("invalid TeeBootstrapV2 magic"));
    }
    let policy = TeePolicyV1::decode_canonical(
        decoder.bounded_bytes("TEE policy", MAX_EVIDENCE_CALL_FRAMING_BYTES)?,
    )?;
    let committee_snapshot_hash = B256::from(decoder.array::<32>()?);
    let committee_snapshot_block = decoder.u64()?;
    let key_epoch = decoder.u64()?;
    let tribute_offer_epoch = decoder.u64()?;
    let dkg_transcript_hash = B256::from(decoder.array::<32>()?);
    let tribute_offer_public_key = B256::from(decoder.array::<32>()?);
    let tribute_offer_group_public_key = Bytes::copy_from_slice(
        decoder.bounded_bytes("TEE bootstrap group public key", MAX_GROUP_PUBLIC_KEY_BYTES)?,
    );

    let collateral_pool = decode_collateral(&mut decoder)?;
    let participants = decode_participants(&mut decoder)?;
    let committee_signatures = decode_signatures(&mut decoder)?;
    decoder.finish()?;

    let value = TeeBootstrapV2 {
        authority: TeeBootstrapAuthorityV2 {
            policy,
            committee_snapshot_hash,
            committee_snapshot_block,
            key_epoch,
            tribute_offer_epoch,
            dkg_transcript_hash,
            tribute_offer_public_key,
            tribute_offer_group_public_key,
        },
        collateral_pool,
        participants,
        committee_signatures,
    };
    value.validate()?;
    Ok(value)
}

fn decode_collateral(
    decoder: &mut Decoder<'_>,
) -> Result<Vec<DcapCollateralComponentV1>, CodecError> {
    let count = decoder.bounded_count_u16(
        "TEE bootstrap collateral pool",
        MAX_COLLATERAL_POOL_COMPONENTS,
    )?;
    let mut pool = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = DcapCollateralKind::try_from(decoder.u8()?)?;
        let bytes = decoder
            .bounded_bytes("DCAP collateral component", MAX_COLLATERAL_COMPONENT_BYTES)?
            .to_vec();
        pool.push(DcapCollateralComponentV1 { kind, bytes });
    }
    Ok(pool)
}

fn decode_participants(
    decoder: &mut Decoder<'_>,
) -> Result<Vec<TeeBootstrapParticipantV2>, CodecError> {
    let count =
        decoder.bounded_count_u16("TEE bootstrap participants", MAX_BOOTSTRAP_PARTICIPANTS)?;
    let mut participants = Vec::with_capacity(count);
    for _ in 0..count {
        participants.push(decode_participant(decoder)?);
    }
    Ok(participants)
}

fn decode_participant(decoder: &mut Decoder<'_>) -> Result<TeeBootstrapParticipantV2, CodecError> {
    let intent = RegistrationIntentV1::decode_canonical(
        decoder.bounded_bytes("registration intent", MAX_EVIDENCE_CALL_FRAMING_BYTES)?,
    )?;
    let validator_binding = ValidatorNodeBindingV1::decode_canonical(
        decoder.take(ValidatorNodeBindingV1::CANONICAL_LEN)?,
    )?;
    let validator_signature = decoder.array()?;
    let node_binding_signature = decoder.array()?;
    let evidence = decode_evidence(decoder)?;
    Ok(TeeBootstrapParticipantV2 {
        intent,
        validator_binding,
        validator_signature,
        node_binding_signature,
        evidence,
        node_signature: decoder.array()?,
        enclave_signature: decoder.array()?,
    })
}

fn decode_evidence(
    decoder: &mut Decoder<'_>,
) -> Result<TeeBootstrapParticipantEvidenceV2, CodecError> {
    let mode = AttestationMode::decode(decoder.u8()?)?;
    match mode {
        AttestationMode::DcapRequired => {
            let quote = decoder
                .bounded_bytes("SGX quote", MAX_QUOTE_BYTES)?
                .to_vec();
            let mut collateral_component_indices = [0_u16; 8];
            for index in &mut collateral_component_indices {
                *index = decoder.u16()?;
            }
            Ok(TeeBootstrapParticipantEvidenceV2::Dcap {
                quote,
                collateral_component_indices,
            })
        }
        AttestationMode::GramineDirectDev => {
            Ok(TeeBootstrapParticipantEvidenceV2::GramineDirectDev {
                dev_attestation_public: decoder.array()?,
                dev_signature: decoder.array()?,
            })
        }
    }
}

fn decode_signatures(
    decoder: &mut Decoder<'_>,
) -> Result<Vec<TeeBootstrapCommitteeSignatureV2>, CodecError> {
    let count = decoder.bounded_count_u16(
        "TEE bootstrap committee signatures",
        MAX_BOOTSTRAP_PARTICIPANTS,
    )?;
    let mut signatures = Vec::with_capacity(count);
    for _ in 0..count {
        signatures.push(TeeBootstrapCommitteeSignatureV2 {
            validator: Address::from(decoder.array::<20>()?),
            signature: decoder.array()?,
        });
    }
    Ok(signatures)
}

pub(super) fn logical_evidence(
    payload: &TeeBootstrapV2,
    participant_index: usize,
) -> Result<AttestationEvidenceV1, CodecError> {
    let participant =
        payload
            .participants
            .get(participant_index)
            .ok_or(CodecError::NonCanonical(
                "TEE bootstrap participant index is out of range",
            ))?;
    let evidence = match &participant.evidence {
        TeeBootstrapParticipantEvidenceV2::Dcap {
            quote,
            collateral_component_indices,
        } => {
            let components =
                reconstruct_collateral(&payload.collateral_pool, collateral_component_indices)?;
            AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                intent: participant.intent.clone(),
                quote: quote.clone(),
                components,
                transition_key_ready_proof: None,
            })
        }
        TeeBootstrapParticipantEvidenceV2::GramineDirectDev {
            dev_attestation_public,
            dev_signature,
        } => AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
            transition_key_ready_proof: None,
            intent: participant.intent.clone(),
            dev_attestation_public: *dev_attestation_public,
            dev_signature: *dev_signature,
        }),
    };
    evidence.encode_canonical()?;
    Ok(evidence)
}

fn reconstruct_collateral(
    pool: &[DcapCollateralComponentV1],
    indices: &[u16; 8],
) -> Result<Vec<DcapCollateralComponentV1>, CodecError> {
    let mut components = Vec::with_capacity(8);
    for (expected_kind, index) in indices.iter().enumerate() {
        let component = pool
            .get(usize::from(*index))
            .ok_or(CodecError::NonCanonical(
                "TEE bootstrap collateral reference is out of range",
            ))?;
        if component.kind as u8 != (expected_kind + 1) as u8 {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap collateral reference kind mismatch",
            ));
        }
        components.push(component.clone());
    }
    Ok(components)
}
