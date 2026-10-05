//! Deterministic submission ordering, owned collateral ingestion and canonical reindexing.
use super::*;

type ComponentIndices = BTreeMap<(u8, Vec<u8>), u16>;

/// Assemble the deterministic unsigned body from complete per-validator
/// evidence and the existing DKG/offer-key result. Committee signature
/// records are installed in canonical validator order with zeroed bytes so
/// every node derives the same signing hash; coordination replaces only
/// those excluded signature bytes.
pub fn assemble_unsigned(
    authority: TeeBootstrapAuthorityV2,
    submissions: Vec<TeeBootstrapParticipantSubmissionV2>,
) -> Result<TeeBootstrapV2, CodecError> {
    enforce_limit(
        "TEE bootstrap participants",
        MAX_BOOTSTRAP_PARTICIPANTS,
        submissions.len(),
    )?;
    if submissions.is_empty() {
        return Err(CodecError::NonCanonical(
            "TEE bootstrap participant set is empty",
        ));
    }
    let ordered = order_submissions(authority.policy.attestation_mode, submissions)?;
    let mut component_indices = ComponentIndices::new();
    let mut participants = Vec::with_capacity(ordered.len());
    for submission in ordered {
        participants.push(consume_submission(submission, &mut component_indices)?);
    }
    let (collateral_pool, canonical_indices) = canonicalize_pool(component_indices)?;
    reindex_participants(&mut participants, &canonical_indices)?;
    let committee_signatures = participants
        .iter()
        .map(|participant| {
            Ok(TeeBootstrapCommitteeSignatureV2 {
                validator: Address::from(participant.validator_binding.validator),
                signature: [0; 65],
            })
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    let payload = TeeBootstrapV2 {
        authority,
        collateral_pool,
        participants,
        committee_signatures,
    };
    payload.preflight()?;
    Ok(payload)
}

fn order_submissions(
    mode: AttestationMode,
    submissions: Vec<TeeBootstrapParticipantSubmissionV2>,
) -> Result<Vec<TeeBootstrapParticipantSubmissionV2>, CodecError> {
    let mut ordered = submissions
        .into_iter()
        .map(|submission| {
            if submission.evidence.mode() != mode {
                return Err(CodecError::NonCanonical(
                    "TEE bootstrap submission mode does not match policy",
                ));
            }
            Ok(submission)
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    ordered.sort_by_key(|submission| Address::from(submission.validator_binding.validator));
    Ok(ordered)
}

fn consume_submission(
    submission: TeeBootstrapParticipantSubmissionV2,
    component_indices: &mut ComponentIndices,
) -> Result<TeeBootstrapParticipantV2, CodecError> {
    let TeeBootstrapParticipantSubmissionV2 {
        evidence,
        validator_binding,
        validator_signature,
        node_binding_signature,
        node_signature,
        enclave_signature,
    } = submission;
    let (intent, evidence) = ingest_evidence(evidence, component_indices)?;
    Ok(TeeBootstrapParticipantV2 {
        intent,
        validator_binding,
        validator_signature,
        node_binding_signature,
        evidence,
        node_signature,
        enclave_signature,
    })
}

fn ingest_evidence(
    evidence: AttestationEvidenceV1,
    component_indices: &mut ComponentIndices,
) -> Result<(RegistrationIntentV1, TeeBootstrapParticipantEvidenceV2), CodecError> {
    match evidence {
        AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent,
            quote,
            components,
            transition_key_ready_proof: _,
        }) => {
            let indices = insert_components(components, component_indices)?;
            Ok((
                intent,
                TeeBootstrapParticipantEvidenceV2::Dcap {
                    quote,
                    collateral_component_indices: indices,
                },
            ))
        }
        AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
            transition_key_ready_proof: None,
            intent,
            dev_attestation_public,
            dev_signature,
        }) => Ok((
            intent,
            TeeBootstrapParticipantEvidenceV2::GramineDirectDev {
                dev_attestation_public,
                dev_signature,
            },
        )),
        AttestationEvidenceV1::GramineDirectDev(_) => Err(CodecError::NonCanonical(
            "bootstrap cannot carry transition evidence",
        )),
    }
}

fn insert_components(
    components: Vec<DcapCollateralComponentV1>,
    component_indices: &mut ComponentIndices,
) -> Result<[u16; 8], CodecError> {
    let components: [DcapCollateralComponentV1; 8] = components.try_into().map_err(|_| {
        CodecError::NonCanonical("TEE bootstrap evidence does not contain exactly eight components")
    })?;
    let mut indices = [0_u16; 8];
    // Move the already-owned buffers into deduplication, without an aggregate clone.
    for (expected_kind, component) in components.into_iter().enumerate() {
        if component.kind as u8 != (expected_kind + 1) as u8 {
            return Err(CodecError::NonCanonical(
                "TEE bootstrap collateral component kind mismatch",
            ));
        }
        let next_index =
            u16::try_from(component_indices.len()).map_err(|_| CodecError::ArithmeticOverflow)?;
        indices[expected_kind] = *component_indices
            .entry((component.kind as u8, component.bytes))
            .or_insert(next_index);
    }
    Ok(indices)
}

fn canonicalize_pool(
    indices: ComponentIndices,
) -> Result<(Vec<DcapCollateralComponentV1>, Vec<u16>), CodecError> {
    let mut canonical_by_temporary = vec![0_u16; indices.len()];
    let pool = indices
        .into_iter()
        .enumerate()
        .map(|(canonical_index, ((kind, bytes), temporary_index))| {
            canonical_by_temporary[usize::from(temporary_index)] =
                u16::try_from(canonical_index).map_err(|_| CodecError::ArithmeticOverflow)?;
            Ok(DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind)?,
                bytes,
            })
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    Ok((pool, canonical_by_temporary))
}

fn reindex_participants(
    participants: &mut [TeeBootstrapParticipantV2],
    canonical_by_temporary: &[u16],
) -> Result<(), CodecError> {
    for participant in participants {
        reindex_evidence(&mut participant.evidence, canonical_by_temporary)?;
    }
    Ok(())
}

fn reindex_evidence(
    evidence: &mut TeeBootstrapParticipantEvidenceV2,
    canonical_by_temporary: &[u16],
) -> Result<(), CodecError> {
    if let TeeBootstrapParticipantEvidenceV2::Dcap {
        collateral_component_indices,
        ..
    } = evidence
    {
        for index in collateral_component_indices {
            *index = *canonical_by_temporary
                .get(usize::from(*index))
                .ok_or(CodecError::ArithmeticOverflow)?;
        }
    }
    Ok(())
}
