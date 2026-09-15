//! Exact-block Fidelity and frozen Nod price openings for LYSIS_V1.

use alloy_primitives::B256;
use outbe_nod::openings::entry_price_slots;
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    intent::{job_id_from_intent_id, VerifiedFinalizedIntentV1},
    league_snapshot::ordered_league_snapshot_slots,
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    SchemaLimits,
};
use outbe_primitives::addresses::{METADOSIS_ADDRESS, NOD_ADDRESS};
use reth_provider::StateProviderFactory;

use super::{
    finality::{
        build_verified_raw_contract_opening, build_verified_raw_contract_opening_from_public_proof,
        verify_raw_contract_opening, PublicExactBlockProofSourceV1,
    },
    retention::{CandidatePinV1, RetentionError},
};

/// Builds the exact Lysis openings exclusively from standard public
/// `eth_getProof` data at the authenticated request block.
///
/// Both slot plans are derived directly from the subjects. The legacy
/// `oracle` wire field carries the Nod entry-price snapshot proof.
pub fn build_public_lysis_openings<S>(
    source: &S,
    finalized: &VerifiedFinalizedIntentV1,
    subjects: OpeningSubjectsV1,
    limits: &SchemaLimits,
) -> Result<LysisOpeningsProofV1, RetentionError>
where
    S: PublicExactBlockProofSourceV1,
{
    validate_subjects(&subjects)?;
    let state_root = finalized.request.state_root;
    let block_hash = finalized.request.block_hash;
    let fidelity_slots =
        fidelity_league_slots(&subjects, finalized.intent.wwd, limits.max_collection_items)?;
    let fidelity_proof = source
        .account_proof(METADOSIS_ADDRESS, &fidelity_slots, block_hash)
        .map_err(|error| RetentionError::Source(format!("read public Fidelity proof: {error}")))?;
    let fidelity = build_verified_raw_contract_opening_from_public_proof(
        &fidelity_proof,
        state_root,
        METADOSIS_ADDRESS,
        &fidelity_slots,
        limits,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;

    let day = outbe_primitives::time::WorldwideDay::new(finalized.intent.wwd);
    let oracle_slots = entry_price_slots(day, &subjects.reference_isos)
        .map_err(|error| RetentionError::Source(error.to_string()))?;
    let oracle_proof = source
        .account_proof(NOD_ADDRESS, &oracle_slots, block_hash)
        .map_err(|error| RetentionError::Source(format!("read public Nod price proof: {error}")))?;
    let oracle = build_verified_raw_contract_opening_from_public_proof(
        &oracle_proof,
        state_root,
        NOD_ADDRESS,
        &oracle_slots,
        limits,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;

    let openings = LysisOpeningsProofV1 {
        protocol_bundle_hash: finalized.intent.protocol_bundle_hash,
        job_id: finalized.job_id,
        finalized_block_hash: block_hash,
        finalized_state_root: state_root,
        wwd: finalized.intent.wwd,
        subjects,
        fidelity,
        oracle,
    };
    verify_lysis_openings(&openings, finalized, &openings.subjects, limits)?;
    Ok(openings)
}

pub fn build_lysis_openings<P>(
    provider: &P,
    limits: &SchemaLimits,
    candidate: CandidatePinV1,
    subjects: OpeningSubjectsV1,
) -> Result<LysisOpeningsProofV1, RetentionError>
where
    P: StateProviderFactory + Send + Sync,
{
    validate_subjects(&subjects)?;
    let state = provider
        .state_by_block_hash(candidate.block_hash)
        .map_err(|error| {
            RetentionError::Source(format!(
                "open exact block state for Lysis openings: {error}"
            ))
        })?;

    let fidelity_slots =
        fidelity_league_slots(&subjects, candidate.wwd, limits.max_collection_items)?;
    let oracle_slots = entry_price_slots(
        outbe_primitives::time::WorldwideDay::new(candidate.wwd),
        &subjects.reference_isos,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;
    let fidelity = build_verified_raw_contract_opening(
        state.as_ref(),
        candidate.state_root,
        METADOSIS_ADDRESS,
        &fidelity_slots,
        limits,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;
    let oracle = build_verified_raw_contract_opening(
        state.as_ref(),
        candidate.state_root,
        NOD_ADDRESS,
        &oracle_slots,
        limits,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;
    let job_id = job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .map_err(|error| RetentionError::Source(format!("derive opening JobId: {error}")))?;

    let openings = LysisOpeningsProofV1 {
        protocol_bundle_hash: candidate.protocol_bundle_hash,
        job_id,
        finalized_block_hash: candidate.block_hash,
        finalized_state_root: candidate.state_root,
        wwd: candidate.wwd,
        subjects,
        fidelity,
        oracle,
    };
    openings
        .validate_profile(limits)
        .map_err(|error| RetentionError::Source(error.to_string()))?;
    Ok(openings)
}

/// Verifies the complete historical Fidelity/Oracle input returned by the node.
///
/// The first proof pass authenticates every supplied raw value against the
/// finalized state root. Only then are the count-dependent canonical slot plans
/// reconstructed and compared with the supplied slot order.
pub fn verify_lysis_openings(
    openings: &LysisOpeningsProofV1,
    finalized: &VerifiedFinalizedIntentV1,
    expected_subjects: &OpeningSubjectsV1,
    limits: &SchemaLimits,
) -> Result<(), RetentionError> {
    openings
        .validate_profile(limits)
        .map_err(|error| RetentionError::Source(error.to_string()))?;
    if openings.protocol_bundle_hash != finalized.intent.protocol_bundle_hash
        || openings.job_id != finalized.job_id
        || openings.finalized_block_hash != finalized.request.block_hash
        || openings.finalized_state_root != finalized.request.state_root
        || openings.wwd != finalized.intent.wwd
        || openings.subjects != *expected_subjects
    {
        return Err(RetentionError::Source(
            "Lysis openings do not bind the authenticated finalized job".to_owned(),
        ));
    }

    let supplied_fidelity_slots = openings
        .fidelity
        .ordered_slots
        .iter()
        .map(|raw| raw.slot)
        .collect::<Vec<_>>();
    verify_raw_contract_opening(
        &openings.fidelity,
        METADOSIS_ADDRESS,
        finalized.request.state_root,
        &supplied_fidelity_slots,
        limits,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;
    let expected_fidelity_slots =
        ordered_league_snapshot_slots(finalized.intent.wwd, &expected_subjects.owners);
    if supplied_fidelity_slots != expected_fidelity_slots {
        return Err(RetentionError::Source(
            "Fidelity league opening does not match the canonical per-owner snapshot slots"
                .to_owned(),
        ));
    }

    let supplied_oracle_slots = openings
        .oracle
        .ordered_slots
        .iter()
        .map(|raw| raw.slot)
        .collect::<Vec<_>>();
    verify_raw_contract_opening(
        &openings.oracle,
        NOD_ADDRESS,
        finalized.request.state_root,
        &supplied_oracle_slots,
        limits,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;
    let expected_oracle_slots = entry_price_slots(
        outbe_primitives::time::WorldwideDay::new(finalized.intent.wwd),
        &expected_subjects.reference_isos,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;
    if supplied_oracle_slots != expected_oracle_slots {
        return Err(RetentionError::Source(
            "Nod price opening does not match the canonical snapshot slot plan".to_owned(),
        ));
    }
    Ok(())
}

fn validate_subjects(subjects: &OpeningSubjectsV1) -> Result<(), RetentionError> {
    let max_owners =
        usize::try_from(OCOMP_POC_CANDIDATE_LIMITS_V1.max_fidelity_openings_per_work_shard)
            .map_err(|_| {
                RetentionError::Source(
                    "per-work-shard Fidelity opening cap does not fit usize".to_owned(),
                )
            })?;
    if subjects.owners.is_empty()
        || subjects.owners.len() > max_owners
        || subjects.owners.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(RetentionError::Source(
            "opening owners are empty, over cap, duplicated, or unordered".to_owned(),
        ));
    }
    Ok(())
}

/// Canonical per-owner Fidelity league snapshot slots in Metadosis storage.
///
/// Unlike the former raw-cohort plan this reads no contract state: each owner
/// contributes exactly one slot derived purely from `(wwd, owner)`. Owners are
/// already strictly ordered and unique (`validate_subjects`), so the slots are
/// distinct and follow the canonical owner order the verifier reconstructs.
fn fidelity_league_slots(
    subjects: &OpeningSubjectsV1,
    wwd: u32,
    max_slots: usize,
) -> Result<Vec<B256>, RetentionError> {
    if subjects.owners.len() > max_slots {
        return Err(RetentionError::Source(
            "Fidelity league opening slot count exceeds the bounded profile".to_owned(),
        ));
    }
    Ok(ordered_league_snapshot_slots(wwd, &subjects.owners))
}
