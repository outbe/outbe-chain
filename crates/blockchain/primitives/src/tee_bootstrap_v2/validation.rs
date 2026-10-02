//! Ordered bootstrap admission checks; aggregate limits precede evidence allocation.
use super::*;

pub(super) fn validate(payload: &TeeBootstrapV2) -> Result<(), CodecError> {
    let policy_hash = payload.policy.policy_hash()?;
    validate_authority_shape(payload)?;
    validate_collateral_pool(payload)?;
    // Reject aggregate calldata before allocating the used-component bitmap
    // or reconstructing and encoding every participant's logical evidence.
    enforce_full_calldata_cap(payload.canonical_encoded_len()?)?;
    let mut used_components = vec![false; payload.collateral_pool.len()];
    let mut previous_validator = None;
    for (participant_index, participant) in payload.participants.iter().enumerate() {
        let validator = validate_participant_binding(payload, participant, policy_hash)?;
        if previous_validator.is_some_and(|previous| previous >= validator) {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap participants are not strictly sorted and unique",
            ));
        }
        previous_validator = Some(validator);
        if payload.committee_signatures[participant_index].validator != validator {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap committee signatures do not match participants",
            ));
        }
        validate_participant_evidence(payload, participant, &mut used_components)?;
        let evidence = payload.logical_evidence(participant_index)?;
        enforce_limit(
            "attestation evidence",
            MAX_ATTESTATION_EVIDENCE_BYTES,
            evidence.encode_canonical()?.len(),
        )?;
    }
    if used_components.iter().any(|used| !used) {
        return Err(CodecError::NonCanonical(
            "TEE bootstrap collateral pool contains an unused component",
        ));
    }
    Ok(())
}

fn has_committee_snapshot(payload: &TeeBootstrapV2) -> bool {
    !payload.committee_snapshot_hash.is_zero() && payload.committee_snapshot_block != 0
}

fn has_offer_keys(payload: &TeeBootstrapV2) -> bool {
    !payload.tribute_offer_public_key.is_zero()
        && !payload.tribute_offer_group_public_key.is_empty()
}

fn validate_authority_shape(payload: &TeeBootstrapV2) -> Result<(), CodecError> {
    if !has_committee_snapshot(payload) || !has_offer_keys(payload) {
        return Err(CodecError::NonCanonical(
            "TEE bootstrap contains a zero required authority",
        ));
    }
    enforce_limit(
        "TEE bootstrap group public key",
        MAX_GROUP_PUBLIC_KEY_BYTES,
        payload.tribute_offer_group_public_key.len(),
    )?;
    enforce_limit(
        "TEE bootstrap collateral pool",
        MAX_COLLATERAL_POOL_COMPONENTS,
        payload.collateral_pool.len(),
    )?;
    enforce_limit(
        "TEE bootstrap participants",
        MAX_BOOTSTRAP_PARTICIPANTS,
        payload.participants.len(),
    )?;
    if payload.participants.is_empty() {
        return Err(CodecError::NonCanonical(
            "TEE bootstrap participant set is empty",
        ));
    }
    if payload.committee_signatures.len() != payload.participants.len() {
        return Err(CodecError::NonCanonical(
            "TEE bootstrap signatures do not cover every participant",
        ));
    }
    match payload.policy.attestation_mode {
        AttestationMode::DcapRequired if payload.collateral_pool.is_empty() => {
            return Err(CodecError::NonCanonical(
                "DCAP TEE bootstrap collateral pool is empty",
            ));
        }
        AttestationMode::GramineDirectDev if !payload.collateral_pool.is_empty() => {
            return Err(CodecError::NonCanonical(
                "GramineDirectDev TEE bootstrap must not carry DCAP collateral",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_collateral_pool(payload: &TeeBootstrapV2) -> Result<(), CodecError> {
    for component in &payload.collateral_pool {
        if component.bytes.is_empty() {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap collateral component is empty",
            ));
        }
        enforce_limit(
            "DCAP collateral component",
            MAX_COLLATERAL_COMPONENT_BYTES,
            component.bytes.len(),
        )?;
    }
    for pair in payload.collateral_pool.windows(2) {
        if compare_components(&pair[0], &pair[1]) != Ordering::Less {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap collateral pool is not strictly sorted and unique",
            ));
        }
    }
    Ok(())
}

fn binding_matches_intent(participant: &TeeBootstrapParticipantV2) -> Result<bool, CodecError> {
    Ok(
        participant.validator_binding.chain_id == participant.intent.chain_id
            && participant.validator_binding.genesis_hash == participant.intent.genesis_hash
            && participant.validator_binding.node_id_hash
                == participant.intent.node_id.node_id_hash()?,
    )
}

fn validate_participant_binding(
    payload: &TeeBootstrapV2,
    participant: &TeeBootstrapParticipantV2,
    policy_hash: B256,
) -> Result<Address, CodecError> {
    if participant.intent.operation != AttestationOperationV1::RegisterEnclave
        || participant.intent.attestation_mode != payload.policy.attestation_mode
        || participant.intent.policy_hash != policy_hash
    {
        return Err(CodecError::NonCanonical(
            "TEE bootstrap participant intent is not a policy-bound NodeHost registration",
        ));
    }
    let validator = Address::from(participant.validator_binding.validator);
    if !binding_matches_intent(participant)?
        || !participant
            .validator_binding
            .verify_validator_signature(&participant.validator_signature)
        || !participant
            .validator_binding
            .verify_node_signature(&participant.node_binding_signature)
    {
        return Err(CodecError::NonCanonical(
            "TEE bootstrap validator NodeHost binding is invalid",
        ));
    }
    Ok(validator)
}

fn validate_participant_evidence(
    payload: &TeeBootstrapV2,
    participant: &TeeBootstrapParticipantV2,
    used_components: &mut [bool],
) -> Result<(), CodecError> {
    match &participant.evidence {
        TeeBootstrapParticipantEvidenceV2::Dcap {
            quote,
            collateral_component_indices,
        } if payload.policy.attestation_mode == AttestationMode::DcapRequired => {
            validate_dcap_evidence(
                payload,
                quote,
                collateral_component_indices,
                used_components,
            )?;
        }
        TeeBootstrapParticipantEvidenceV2::GramineDirectDev {
            dev_attestation_public,
            dev_signature,
        } if payload.policy.attestation_mode == AttestationMode::GramineDirectDev => {
            if dev_attestation_public != &participant.intent.attestation_ed25519
                || dev_signature != &participant.enclave_signature
                || !participant.intent.verify_enclave_signature(dev_signature)
            {
                return Err(CodecError::NonCanonical(
                    "GramineDirectDev evidence does not prove the intent attestation key",
                ));
            }
        }
        _ => {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap participant evidence mode does not match policy",
            ));
        }
    }
    Ok(())
}

fn validate_dcap_evidence(
    payload: &TeeBootstrapV2,
    quote: &[u8],
    indices: &[u16; 8],
    used_components: &mut [bool],
) -> Result<(), CodecError> {
    if quote.is_empty() {
        return Err(CodecError::NonCanonical("TEE bootstrap quote is empty"));
    }
    enforce_limit("SGX quote", MAX_QUOTE_BYTES, quote.len())?;
    for (expected_kind, index) in indices.iter().enumerate() {
        let index = usize::from(*index);
        let component = payload
            .collateral_pool
            .get(index)
            .ok_or(CodecError::NonCanonical(
                "TEE bootstrap collateral reference is out of range",
            ))?;
        if component.kind as u8 != (expected_kind + 1) as u8 {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap collateral reference kind mismatch",
            ));
        }
        used_components[index] = true;
    }
    Ok(())
}
