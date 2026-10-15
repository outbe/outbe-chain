//! Exact-block Fidelity and Oracle entry-price openings for LYSIS_V1.

use alloy_primitives::B256;
use outbe_nod::openings::{
    entry_price_day, entry_price_slots, evaluate_entry_prices, pair_index_slots,
};
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    intent::{job_id_from_intent_id, VerifiedFinalizedIntentV1},
    league_snapshot::ordered_league_snapshot_slots,
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    SchemaLimits,
};
use outbe_primitives::addresses::{METADOSIS_ADDRESS, ORACLE_ADDRESS};
use reth_provider::StateProviderFactory;
use reth_storage_api::StateProvider;

use super::{
    finality::{build_verified_raw_contract_opening, verify_raw_contract_opening},
    retention::{CandidatePinV1, RetentionError},
};

/// `logical_evaluation_time` is the request block's timestamp, which selects the priced UTC day.
pub fn build_lysis_openings<P>(
    provider: &P,
    limits: &SchemaLimits,
    candidate: CandidatePinV1,
    logical_evaluation_time: u64,
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
    let oracle_slots = oracle_entry_price_slots(
        state.as_ref(),
        entry_price_day(logical_evaluation_time),
        &subjects.reference_isos,
    )?;
    let fidelity = build_verified_raw_contract_opening(
        state.as_ref(),
        candidate.state_root,
        METADOSIS_ADDRESS,
        &fidelity_slots,
        limits,
    )
    .map_err(RetentionError::source)?;
    let oracle = build_verified_raw_contract_opening(
        state.as_ref(),
        candidate.state_root,
        ORACLE_ADDRESS,
        &oracle_slots,
        limits,
    )
    .map_err(RetentionError::source)?;
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
        .map_err(RetentionError::source)?;
    Ok(openings)
}

/// Verifies the complete historical Fidelity/Oracle input returned by the node.
///
/// For each opening (Fidelity, then Oracle), the function authenticates every
/// supplied raw value against the finalized state root. Then it compares the
/// supplied slot order with the canonical slot plan. The Fidelity plan comes
/// from `wwd` and the subjects; the Oracle plan also follows the proven
/// pair-index words.
pub fn verify_lysis_openings(
    openings: &LysisOpeningsProofV1,
    finalized: &VerifiedFinalizedIntentV1,
    expected_subjects: &OpeningSubjectsV1,
    limits: &SchemaLimits,
) -> Result<(), RetentionError> {
    openings
        .validate_profile(limits)
        .map_err(RetentionError::source)?;
    let finalized_job_mismatch = openings.protocol_bundle_hash
        != finalized.intent.protocol_bundle_hash
        || openings.job_id != finalized.job_id
        || openings.finalized_block_hash != finalized.request.block_hash;
    if finalized_job_mismatch
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
    .map_err(RetentionError::source)?;
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
        ORACLE_ADDRESS,
        finalized.request.state_root,
        &supplied_oracle_slots,
        limits,
    )
    .map_err(RetentionError::source)?;
    let opened = openings
        .oracle
        .ordered_slots
        .iter()
        .map(|raw| (raw.slot, raw.value))
        .collect::<Vec<_>>();
    evaluate_entry_prices(
        entry_price_day(finalized.intent.logical_evaluation_time),
        &expected_subjects.reference_isos,
        &opened,
    )
    .map_err(|_| {
        RetentionError::Source(
            "Oracle entry-price opening does not match the canonical slot plan".to_owned(),
        )
    })?;
    Ok(())
}

/// Reads each subject pair's registry index, which addresses its day-VWAP slot.
fn oracle_entry_price_slots(
    state: &dyn StateProvider,
    day: u32,
    isos: &[u16],
) -> Result<Vec<B256>, RetentionError> {
    let pair_indices = pair_index_slots(isos)
        .map_err(RetentionError::source)?
        .into_iter()
        .map(|slot| {
            let index = state
                .storage(ORACLE_ADDRESS, slot)
                .map_err(|error| {
                    RetentionError::Source(format!("read Oracle pair index: {error}"))
                })?
                .unwrap_or_default();
            u32::try_from(index).map_err(|_| {
                RetentionError::Source("Oracle pair index does not fit u32".to_owned())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entry_price_slots(day, isos, &pair_indices).map_err(RetentionError::source)
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
