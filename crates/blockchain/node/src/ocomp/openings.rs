//! Exact-block Fidelity and Oracle raw opening construction for LYSIS_V1.

use std::collections::BTreeSet;

use alloy_primitives::{Address, B256, U256};
use outbe_fidelity::{fidelity_count_slot_plan_v1, fidelity_opening_slot_plan_v1};
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    intent::job_id_from_intent_id,
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    SchemaLimits,
};
use outbe_oracle::{oracle_count_slot_plan_v1, oracle_opening_slot_plan_v1};
use outbe_primitives::addresses::{FIDELITY_ADDRESS, ORACLE_ADDRESS};
use reth_provider::StateProviderFactory;
use reth_storage_api::StateProvider;

use super::{
    finality::build_verified_raw_contract_opening,
    retention::{CandidatePinV1, RetentionError},
};

pub(super) fn build_lysis_openings<P>(
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

    let fidelity_slots = fidelity_slots(state.as_ref(), &subjects, limits.max_collection_items)?;
    let oracle_slots = oracle_slots(state.as_ref(), candidate, &subjects)?;
    let fidelity = build_verified_raw_contract_opening(
        state.as_ref(),
        candidate.state_root,
        FIDELITY_ADDRESS,
        &fidelity_slots,
        limits,
    )
    .map_err(|error| RetentionError::Source(error.to_string()))?;
    let oracle = build_verified_raw_contract_opening(
        state.as_ref(),
        candidate.state_root,
        ORACLE_ADDRESS,
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

fn fidelity_slots(
    state: &dyn StateProvider,
    subjects: &OpeningSubjectsV1,
    max_slots: usize,
) -> Result<Vec<B256>, RetentionError> {
    let mut slots = Vec::new();
    let mut unique = BTreeSet::new();
    for owner in &subjects.owners {
        let counts = fidelity_count_slot_plan_v1(*owner);
        let active_count = read_u32(
            state,
            FIDELITY_ADDRESS,
            counts.active_count,
            "Fidelity active_count",
        )?;
        let sold_count = read_u32(
            state,
            FIDELITY_ADDRESS,
            counts.sold_count,
            "Fidelity sold_count",
        )?;
        let plan = fidelity_opening_slot_plan_v1(*owner, active_count, sold_count)
            .map_err(|error| RetentionError::Source(error.to_string()))?;
        let additional_slots = plan
            .slots
            .iter()
            .filter(|slot| !unique.contains(*slot))
            .count();
        if slots
            .len()
            .checked_add(additional_slots)
            .is_none_or(|total| total > max_slots)
        {
            return Err(RetentionError::Source(
                "Fidelity raw opening slot count exceeds the bounded profile".to_owned(),
            ));
        }
        for slot in plan.slots {
            if unique.insert(slot) {
                slots.push(slot);
            }
        }
    }
    Ok(slots)
}

fn oracle_slots(
    state: &dyn StateProvider,
    candidate: CandidatePinV1,
    subjects: &OpeningSubjectsV1,
) -> Result<Vec<B256>, RetentionError> {
    let day = outbe_common::WorldwideDay::new(candidate.wwd);
    let counts = oracle_count_slot_plan_v1(day, &subjects.settlement_isos)
        .map_err(|error| RetentionError::Source(error.to_string()))?;
    let settlement_pairs = subjects
        .settlement_isos
        .iter()
        .enumerate()
        .map(|(index, iso)| {
            let pair_slot = counts.slots[index * 2 + 1];
            read_word(state, ORACLE_ADDRESS, pair_slot, "Oracle settlement pair")
                .map(|word| (*iso, B256::new(word.to_be_bytes())))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let count_base = subjects.settlement_isos.len() * 2;
    let worldwide_day_pair_count = read_u32(
        state,
        ORACLE_ADDRESS,
        counts.slots[count_base + 1],
        "Oracle WWD VWAP pair count",
    )?;
    let scurve_count = read_u32(
        state,
        ORACLE_ADDRESS,
        counts.slots[count_base + 2],
        "Oracle S-curve count",
    )?;
    let scurve_oldest = read_u32(
        state,
        ORACLE_ADDRESS,
        counts.slots[count_base + 3],
        "Oracle S-curve oldest",
    )?;
    oracle_opening_slot_plan_v1(
        day,
        &settlement_pairs,
        worldwide_day_pair_count,
        scurve_count,
        scurve_oldest,
    )
    .map(|plan| plan.slots)
    .map_err(|error| RetentionError::Source(error.to_string()))
}

fn read_u32(
    state: &dyn StateProvider,
    address: Address,
    slot: B256,
    field: &'static str,
) -> Result<u32, RetentionError> {
    let word = read_word(state, address, slot, field)?;
    if word > U256::from(u32::MAX) {
        return Err(RetentionError::Source(format!(
            "{field} does not fit canonical u32"
        )));
    }
    Ok(word.to::<u32>())
}

fn read_word(
    state: &dyn StateProvider,
    address: Address,
    slot: B256,
    field: &'static str,
) -> Result<U256, RetentionError> {
    state
        .storage(address, slot)
        .map(|value| value.unwrap_or_default())
        .map_err(|error| RetentionError::Source(format!("read {field}: {error}")))
}
