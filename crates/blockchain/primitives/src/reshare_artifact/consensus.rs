//! Consensus record payloads, including the canonical DKG boundary format.
use super::{
    ensure_count_fits_u16, ensure_len_fits_u32, ensure_payload_fits_u16,
    tee_expired_target_exclusions_hash, validate_tee_expired_target_exclusions, Address, Bytes,
    ConsensusHeaderArtifact, DkgBoundaryArtifact, ReshareArtifactError, Result, B256, BOUNDARY_TAG,
    COMMITTEE_PREANNOUNCE_TAG, DEALER_LOG_TAG, MAX_TEE_EXPIRED_TARGET_EXCLUSIONS,
};
use crate::consensus::ReshareResult;

pub(super) fn encode_record(artifact: &ConsensusHeaderArtifact) -> Result<(u8, Vec<u8>)> {
    match artifact {
        ConsensusHeaderArtifact::BoundaryOutcome(result) => {
            Ok((BOUNDARY_TAG, encode_boundary_payload(result)?))
        }
        ConsensusHeaderArtifact::DealerLog(log) => {
            ensure_payload_fits_u16("dealer log", log.len())?;
            Ok((DEALER_LOG_TAG, log.to_vec()))
        }
        ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome } => {
            let mut payload = Vec::with_capacity(8 + outcome.len());
            payload.extend_from_slice(&epoch.to_be_bytes());
            payload.extend_from_slice(outcome.as_ref());
            ensure_payload_fits_u16("committee pre-announce", payload.len())?;
            Ok((COMMITTEE_PREANNOUNCE_TAG, payload))
        }
    }
}

pub(super) fn decode_boundary_record(payload: &[u8]) -> Result<ConsensusHeaderArtifact> {
    Ok(ConsensusHeaderArtifact::BoundaryOutcome(
        decode_boundary_payload(payload)?,
    ))
}

pub(super) fn decode_preannounce_record(payload: &[u8]) -> Result<ConsensusHeaderArtifact> {
    if payload.len() < 8 {
        return Err(ReshareArtifactError::CommitteePreAnnouncePayloadTooShort.into());
    }
    let mut epoch_buf = [0u8; 8];
    epoch_buf.copy_from_slice(&payload[..8]);
    Ok(ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: u64::from_be_bytes(epoch_buf),
        outcome: Bytes::copy_from_slice(&payload[8..]),
    })
}

fn encode_boundary_payload(result: &DkgBoundaryArtifact) -> Result<Vec<u8>> {
    ensure_count_fits_u16("reshare active set", result.reshare.new_active_set.len())?;
    ensure_len_fits_u32("boundary outcome", result.outcome.len())?;

    ensure_len_fits_u32(
        "boundary vrf group public key",
        result.vrf_group_public_key_bytes.len(),
    )?;
    ensure_count_fits_u16("tee recipient pubkeys", result.tee_recipient_pubkeys.len())?;
    validate_tee_expired_target_exclusions(&result.tee_expired_target_exclusions)?;
    let exclusions_hash =
        tee_expired_target_exclusions_hash(&result.tee_expired_target_exclusions)?;
    if exclusions_hash != result.tee_expired_target_exclusions_hash {
        return Err(ReshareArtifactError::BoundaryTeeExpiryExclusionsCommitmentMismatch.into());
    }

    let mut payload = Vec::with_capacity(
        8 + 8
            + 8
            + 8
            + 32
            + 8
            + 32
            + 32 // V2 committee_set_hash
            + 1
            + 1
            + 32
            + 2
            + (result.reshare.new_active_set.len() * 20)
            + 4
            + result.outcome.len()
            + 4 // V2 vrf_group_public_key_bytes length prefix
            + result.vrf_group_public_key_bytes.len()
            + 2 // V0.07 tee_recipient_pubkeys count
            + (result.tee_recipient_pubkeys.len() * (20 + 32))
            + 32 // V0.0B expiry exclusions commitment
            + 2 // V0.0B expiry exclusions count
            + (result.tee_expired_target_exclusions.len() * 20),
    );
    payload.extend_from_slice(&result.epoch.to_be_bytes());
    payload.extend_from_slice(&result.dkg_cycle.to_be_bytes());
    payload.extend_from_slice(&result.freeze_height.to_be_bytes());
    payload.extend_from_slice(&result.planned_activation_height.to_be_bytes());
    payload.extend_from_slice(result.target_set_hash.as_slice());
    payload.extend_from_slice(&result.vrf_material_version.to_be_bytes());
    payload.extend_from_slice(result.vrf_group_public_key.as_slice());
    payload.extend_from_slice(result.committee_set_hash.as_slice());
    payload.push(u8::from(result.is_validator_set_change));
    payload.push(u8::from(result.is_full_dkg));
    payload.extend_from_slice(result.reshare.active_set_hash.as_slice());
    payload.extend_from_slice(&(result.reshare.new_active_set.len() as u16).to_be_bytes());
    for address in &result.reshare.new_active_set {
        payload.extend_from_slice(address.as_slice());
    }
    payload.extend_from_slice(&(result.outcome.len() as u32).to_be_bytes());
    payload.extend_from_slice(result.outcome.as_ref());
    payload.extend_from_slice(&(result.vrf_group_public_key_bytes.len() as u32).to_be_bytes());
    payload.extend_from_slice(result.vrf_group_public_key_bytes.as_ref());
    payload.extend_from_slice(&(result.tee_recipient_pubkeys.len() as u16).to_be_bytes());
    for (address, recipient_pubkey) in &result.tee_recipient_pubkeys {
        payload.extend_from_slice(address.as_slice());
        payload.extend_from_slice(recipient_pubkey.as_slice());
    }
    // V0.0B: exact freeze-height TEE expiry exclusions. The payload carries the
    // explicit commitment with the ordered unique list, so proposal equality
    // and execution bind the same authority.
    payload.extend_from_slice(result.tee_expired_target_exclusions_hash.as_slice());
    payload.extend_from_slice(&(result.tee_expired_target_exclusions.len() as u16).to_be_bytes());
    for address in &result.tee_expired_target_exclusions {
        payload.extend_from_slice(address.as_slice());
    }
    Ok(payload)
}

pub(super) fn decode_boundary_payload(payload: &[u8]) -> Result<DkgBoundaryArtifact> {
    // Minimum boundary payload: epoch+dkg_cycle+freeze+planned (4*u64) + target_set_hash (32)
    // + vrf_material_version (u64) + vrf_group_public_key (32) + committee_set_hash (32)
    // + is_validator_set_change (1) + is_full_dkg (1) + active_set_hash (32) + count (u16)
    // + outcome_len (u32) + vrf_group_pk_len (u32).
    if payload.len() < 8 + 8 + 8 + 8 + 32 + 8 + 32 + 32 + 1 + 1 + 32 + 2 + 4 + 4 {
        return Err(ReshareArtifactError::BoundaryPayloadTooShort.into());
    }

    let mut offset = 0usize;
    let epoch = read_u64(payload, &mut offset, "boundary epoch")?;
    let dkg_cycle = read_u64(payload, &mut offset, "boundary dkg cycle")?;
    let freeze_height = read_u64(payload, &mut offset, "boundary freeze height")?;
    let planned_activation_height =
        read_u64(payload, &mut offset, "boundary planned activation height")?;

    let target_set_hash = B256::from_slice(&payload[offset..offset + 32]);
    offset += 32;

    let vrf_material_version = read_u64(payload, &mut offset, "boundary vrf material version")?;

    let vrf_group_public_key = B256::from_slice(&payload[offset..offset + 32]);
    offset += 32;

    let committee_set_hash = B256::from_slice(&payload[offset..offset + 32]);
    offset += 32;

    let is_validator_set_change =
        decode_boundary_flag(payload, &mut offset, "is_validator_set_change")?;

    let is_full_dkg = decode_boundary_flag(payload, &mut offset, "is_full_dkg")?;

    let active_set_hash = B256::from_slice(&payload[offset..offset + 32]);
    offset += 32;

    let count = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
    offset += 2;
    let addresses_len = count
        .checked_mul(20)
        .ok_or(ReshareArtifactError::BoundaryAddressListLengthOverflow)?;
    let needed_before_outcome = offset + addresses_len + 4;
    if payload.len() < needed_before_outcome {
        return Err(ReshareArtifactError::BoundaryPayloadLengthTooShort {
            actual: payload.len(),
            needed: needed_before_outcome,
        }
        .into());
    }

    let new_active_set = (0..count)
        .map(|index| {
            let start = offset + index * 20;
            Address::from_slice(&payload[start..start + 20])
        })
        .collect();
    offset += addresses_len;

    let outcome_len = u32::from_be_bytes(
        payload[offset..offset + 4]
            .try_into()
            .map_err(|_| ReshareArtifactError::InvalidOutcomeLengthBytes)?,
    ) as usize;
    offset += 4;
    let needed_after_outcome = offset
        .checked_add(outcome_len)
        .and_then(|v| v.checked_add(4))
        .ok_or(ReshareArtifactError::BoundaryPayloadLengthOverflow)?;
    if payload.len() < needed_after_outcome {
        return Err(ReshareArtifactError::BoundaryPayloadLengthTooShort {
            actual: payload.len(),
            needed: needed_after_outcome,
        }
        .into());
    }
    let outcome = Bytes::copy_from_slice(&payload[offset..offset + outcome_len]);
    offset += outcome_len;

    let vrf_group_public_key_bytes = decode_boundary_vrf_key(payload, &mut offset)?;

    let tee_recipient_pubkeys = decode_boundary_recipients(payload, &mut offset)?;

    let (tee_expired_target_exclusions, carried_tee_expired_target_exclusions_hash) =
        decode_boundary_exclusions(payload, &mut offset)?;

    Ok(DkgBoundaryArtifact {
        epoch,
        dkg_cycle,
        freeze_height,
        planned_activation_height,
        target_set_hash,
        vrf_material_version,
        vrf_group_public_key,
        vrf_group_public_key_bytes,
        committee_set_hash,
        is_validator_set_change,
        outcome,
        is_full_dkg,
        reshare: ReshareResult {
            new_active_set,
            active_set_hash,
        },
        tee_recipient_pubkeys,
        tee_expired_target_exclusions,
        tee_expired_target_exclusions_hash: carried_tee_expired_target_exclusions_hash,
    })
}

fn read_u64(payload: &[u8], offset: &mut usize, name: &'static str) -> Result<u64> {
    let end = offset.saturating_add(8);
    let Some(bytes) = payload.get(*offset..end) else {
        return Err(ReshareArtifactError::UnexpectedEofReading { name }.into());
    };
    *offset = end;
    Ok(u64::from_be_bytes(bytes.try_into().map_err(|_| {
        ReshareArtifactError::InvalidFieldBytes { name }
    })?))
}

fn decode_boundary_vrf_key(payload: &[u8], offset: &mut usize) -> Result<Bytes> {
    let vrf_group_pk_len = u32::from_be_bytes(
        payload[*offset..*offset + 4]
            .try_into()
            .map_err(|_| ReshareArtifactError::InvalidVrfGroupPkLengthBytes)?,
    ) as usize;
    *offset += 4;
    let needed_after_vrf = *offset + vrf_group_pk_len;
    if payload.len() < needed_after_vrf {
        return Err(ReshareArtifactError::BoundaryPayloadLengthTooShort {
            actual: payload.len(),
            needed: needed_after_vrf,
        }
        .into());
    }
    let vrf_group_public_key_bytes =
        Bytes::copy_from_slice(&payload[*offset..*offset + vrf_group_pk_len]);
    *offset += vrf_group_pk_len;

    Ok(vrf_group_public_key_bytes)
}

fn decode_boundary_recipients(payload: &[u8], offset: &mut usize) -> Result<Vec<(Address, B256)>> {
    // V0.07: tee_recipient_pubkeys (u16 count + entries of Address(20)+B256(32)).
    if payload.len() < *offset + 2 {
        return Err(ReshareArtifactError::MissingTeeRecipientCount.into());
    }
    let tee_count = u16::from_be_bytes([payload[*offset], payload[*offset + 1]]) as usize;
    *offset += 2;
    let tee_bytes = tee_count
        .checked_mul(20 + 32)
        .ok_or(ReshareArtifactError::TeeRecipientPubkeysLengthOverflow)?;
    let needed_after_recipients = offset
        .checked_add(tee_bytes)
        .and_then(|v| v.checked_add(32 + 2))
        .ok_or(ReshareArtifactError::BoundaryPayloadLengthOverflow)?;
    if payload.len() < needed_after_recipients {
        return Err(ReshareArtifactError::BoundaryPayloadLengthTooShort {
            actual: payload.len(),
            needed: needed_after_recipients,
        }
        .into());
    }
    let mut tee_recipient_pubkeys = Vec::with_capacity(tee_count);
    for _ in 0..tee_count {
        let address = Address::from_slice(&payload[*offset..*offset + 20]);
        *offset += 20;
        let recipient_pubkey = B256::from_slice(&payload[*offset..*offset + 32]);
        *offset += 32;
        tee_recipient_pubkeys.push((address, recipient_pubkey));
    }

    Ok(tee_recipient_pubkeys)
}

fn decode_boundary_exclusions(payload: &[u8], offset: &mut usize) -> Result<(Vec<Address>, B256)> {
    // V0.0B: domain-separated commitment plus bounded ordered unique list.
    let carried_tee_expired_target_exclusions_hash =
        B256::from_slice(&payload[*offset..*offset + 32]);
    *offset += 32;
    let exclusions_count = u16::from_be_bytes([payload[*offset], payload[*offset + 1]]) as usize;
    *offset += 2;
    if exclusions_count > MAX_TEE_EXPIRED_TARGET_EXCLUSIONS {
        return Err(ReshareArtifactError::TeeExpiryExclusionsExceedCap {
            count: exclusions_count,
            max_count: MAX_TEE_EXPIRED_TARGET_EXCLUSIONS,
        }
        .into());
    }
    let exclusions_bytes = exclusions_count
        .checked_mul(20)
        .ok_or(ReshareArtifactError::TeeExpiryExclusionsLengthOverflow)?;
    let needed_after_exclusions = offset
        .checked_add(exclusions_bytes)
        .ok_or(ReshareArtifactError::BoundaryPayloadLengthOverflow)?;
    if payload.len() < needed_after_exclusions {
        return Err(ReshareArtifactError::BoundaryPayloadLengthTooShort {
            actual: payload.len(),
            needed: needed_after_exclusions,
        }
        .into());
    }
    let mut tee_expired_target_exclusions = Vec::with_capacity(exclusions_count);
    for _ in 0..exclusions_count {
        tee_expired_target_exclusions.push(Address::from_slice(&payload[*offset..*offset + 20]));
        *offset += 20;
    }
    let expected_exclusions_hash =
        tee_expired_target_exclusions_hash(&tee_expired_target_exclusions)?;
    if expected_exclusions_hash != carried_tee_expired_target_exclusions_hash {
        return Err(ReshareArtifactError::BoundaryTeeExpiryExclusionsCommitmentMismatch.into());
    }

    if payload.len() != *offset {
        return Err(ReshareArtifactError::BoundaryPayloadLengthMismatch {
            actual: payload.len(),
            expected: *offset,
        }
        .into());
    }

    Ok((
        tee_expired_target_exclusions,
        carried_tee_expired_target_exclusions_hash,
    ))
}

fn decode_boundary_flag(payload: &[u8], offset: &mut usize, field: &'static str) -> Result<bool> {
    let flag = match payload[*offset] {
        0 => false,
        1 => true,
        other => {
            return Err(ReshareArtifactError::InvalidBoundaryFlag {
                field,
                value: other,
            }
            .into())
        }
    };
    *offset += 1;
    Ok(flag)
}
