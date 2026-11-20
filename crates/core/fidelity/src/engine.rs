//! Fidelity cohort engine.
//!
//! Cohort plaintext exists only inside these functions. Every function is a
//! pure transform of its inputs and the resident state key.

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::FIDELITY;
use outbe_tee::resident::{ResidentOfferMaterial, StateKeyHandle};
use zeroize::Zeroizing;

use crate::cipher::{self, CohortTransition};
use crate::cohort::CohortState;
use crate::errors::{CohortError, EngineError};
use crate::protocol::{
    eip191_hash, fidelity_cohort_canonical_hash, fidelity_query_auth_message,
    fidelity_query_canonical_hash, FidelityCohortOp, FidelityCohortRequest, FidelityCohortResult,
    FidelityLeagueEntry, FidelityOpOutcome, FidelityOpSection, FidelityQueryRequest,
    FidelityQueryResult, FidelitySnapshotRequest,
};

/// Derive the resident Fidelity state key from offer material.
pub fn derive_fidelity_state_key(
    material: &ResidentOfferMaterial,
    chain_id: B256,
    epoch: u64,
) -> Result<StateKeyHandle, outbe_core_common::confidential_tee::TeeError> {
    FIDELITY.derive_state_key(material, chain_id, epoch)
}

/// Open `blob` with `view_key` and decode the cohort interior.
pub fn read_state(
    view_key: &StateKeyHandle,
    account: Address,
    blob: &[u8],
) -> Result<(u64, CohortState), EngineError> {
    let (version, interior) = cipher::read_blob(view_key, account, blob)?;
    let interior = Zeroizing::new(interior);
    Ok((version, CohortState::decode(&interior)?))
}

/// Apply the Fidelity section of a Gratis op and return the plaintext receipt.
pub fn apply_cohort_section(
    state_key: &StateKeyHandle,
    account: Address,
    amount: U256,
    section: &FidelityOpSection,
) -> Result<FidelityOpOutcome, EngineError> {
    let view_key = FIDELITY.account_keys.derive_view_key(state_key, account)?;
    let (version, mut state) = read_state(&view_key, account, &section.current_blob)?;
    let utc_day = outbe_system_common::time::utc_day_from_timestamp(section.timestamp);
    let initialized = mutate(&mut state, section.op, amount, utc_day);
    let effective_first = if section.first_qualified_start != 0 {
        section.first_qualified_start
    } else {
        initialized.unwrap_or(0)
    };
    let (_, _, league) = state.evaluate(utc_day, effective_first)?;
    let new_blob = match section.op {
        FidelityCohortOp::Probe => Vec::new(),
        FidelityCohortOp::In | FidelityCohortOp::Out => {
            let padded_state = Zeroizing::new(state.encode_padded()?);
            cipher::write_blob(
                &view_key,
                CohortTransition {
                    account,
                    previous_version: version,
                    previous_blob: &section.current_blob,
                    padded_state: &padded_state,
                },
            )?
        }
    };
    Ok(FidelityOpOutcome {
        new_blob,
        qualified_start_initialized: initialized,
        league,
    })
}

fn mutate(
    state: &mut CohortState,
    op: FidelityCohortOp,
    amount: U256,
    utc_day: u64,
) -> Option<u64> {
    match op {
        FidelityCohortOp::In => state.cohort_in(amount, utc_day),
        FidelityCohortOp::Out => {
            state.cohort_out(amount, utc_day);
            None
        }
        FidelityCohortOp::Probe => None,
    }
}

/// Apply a standalone cohort op and fill the canonical inputs hash.
///
/// The caller signs `attestation_tag`.
pub fn apply_cohort_op(
    state_key: &StateKeyHandle,
    req: &FidelityCohortRequest,
) -> Result<FidelityCohortResult, EngineError> {
    let outcome = apply_cohort_section(state_key, req.account, req.amount, &req.section)?;
    Ok(FidelityCohortResult {
        outcome,
        inputs_canonical_hash: fidelity_cohort_canonical_hash(req),
        attestation_tag: Vec::new(),
    })
}

/// Batch-decrypt the day's owners and return one league per owner, in request order.
pub fn snapshot_leagues(
    state_key: &StateKeyHandle,
    req: &FidelitySnapshotRequest,
) -> Result<Vec<FidelityLeagueEntry>, EngineError> {
    let eval_day = outbe_system_common::time::utc_day_from_timestamp(req.timestamp);
    let mut leagues = Vec::with_capacity(req.entries.len());
    for entry in &req.entries {
        let view_key = FIDELITY
            .account_keys
            .derive_view_key(state_key, entry.owner)?;
        let (_, state) = read_state(&view_key, entry.owner, &entry.cohort_blob)?;
        let (rcfi, _, league) = state.evaluate(eval_day, req.first_qualified_start)?;
        leagues.push(FidelityLeagueEntry {
            owner: entry.owner,
            league,
            rcfi,
        });
    }
    Ok(leagues)
}

/// Owner-authorized RCFI read. The signed authorization is checked here.
pub fn query_index(
    state_key: &StateKeyHandle,
    resident_chain_id: B256,
    req: &FidelityQueryRequest,
) -> Result<FidelityQueryResult, EngineError> {
    authorize(resident_chain_id, req)?;
    let view_key = FIDELITY
        .account_keys
        .derive_view_key(state_key, req.account)?;
    let (_, state) = read_state(&view_key, req.account, &req.cohort_blob)?;
    let query_day = outbe_system_common::time::utc_day_from_timestamp(req.query_timestamp);
    let (rcfi, efficiency, league) = state.evaluate(query_day, req.first_qualified_start)?;
    Ok(FidelityQueryResult {
        rcfi,
        efficiency,
        league,
        inputs_canonical_hash: fidelity_query_canonical_hash(req),
        attestation_tag: Vec::new(),
    })
}

fn authorize(resident_chain_id: B256, req: &FidelityQueryRequest) -> Result<(), CohortError> {
    if req.chain_id != resident_chain_id {
        return Err(CohortError::QueryAuthDifferentChain);
    }
    if req.expiry < req.block_timestamp {
        return Err(CohortError::QueryAuthExpired);
    }
    let Ok(sig65) = <[u8; 65]>::try_from(req.owner_sig.as_slice()) else {
        return Err(CohortError::OwnerSignatureLength);
    };
    let prehash = eip191_hash(&fidelity_query_auth_message(
        resident_chain_id,
        req.account,
        req.expiry,
    ));
    match outbe_primitives::tee_signatures::recover_signer(&prehash, &sig65) {
        Ok(signer) if signer == req.account => Ok(()),
        _ => Err(CohortError::OwnerSignatureAccountMismatch),
    }
}
