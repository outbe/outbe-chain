//! Certified OCOMP contributor-root installation for Intex.
//!
//! Activation stores one constant-size proof authority. It never receives or
//! writes the contributor owner list and exposes no public write selector.

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use outbe_ocomp_protocol::{
    intent::ContributorTargetPreconditionV1,
    receipts::{
        contributor_state_event_digest, ContributorReceiptV1, ContributorStateEventProjectionV1,
        EffectBindingV1,
    },
    SchemaLimits,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::INTEX_ADDRESS,
    error::{PrecompileError, Result},
    storage::{CertifiedLysisActivation, StorageHandle},
};

use crate::{
    api,
    precompile::IIntex,
    schema::{CertifiedContributorGenerationProjection, IntexContract},
};

/// Closed, constant-size contributor owner input derived from the verified
/// Lysis apply plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedContributorRootV1 {
    pub binding: EffectBindingV1,
    pub precondition: ContributorTargetPreconditionV1,
    pub contributor_root: B256,
    pub contributor_count: u32,
    pub eligible_nominal_total: U256,
}

/// Compare-and-set one certified contributor root without per-owner writes.
pub fn install_certified_contributor_root(
    storage: &StorageHandle<'_>,
    capability: &mut CertifiedLysisActivation<'_>,
    input: &CertifiedContributorRootV1,
    limits: &SchemaLimits,
) -> Result<ContributorReceiptV1> {
    validate_input(capability, input)?;

    let current = api::ocomp_contributor_target_projection(
        storage,
        WorldwideDay::new(input.precondition.worldwide_day),
    )?;
    if current.expected_series_version != input.precondition.expected_series_version {
        return Err(revert("certified contributor target version changed"));
    }
    if current.contributor_count != 0 || !current.contributor_total.is_zero() {
        return Err(revert("certified contributor target is not empty"));
    }

    let intex = IntexContract::new(storage.clone());
    if intex
        .ocomp_certified_contributor_generation(WorldwideDay::new(
            input.precondition.worldwide_day,
        ))?
        .is_some()
    {
        return Err(revert("certified contributor root cannot be overwritten"));
    }
    let next_version = current
        .expected_series_version
        .checked_add(1)
        .ok_or_else(|| revert("certified contributor series version overflow"))?;

    let state_projection = ContributorStateEventProjectionV1 {
        worldwide_day: input.precondition.worldwide_day,
        series_version_before: input.precondition.expected_series_version,
        series_version_after: next_version,
        contributor_count: input.contributor_count,
        contributor_root: input.contributor_root,
        eligible_nominal_total: input.eligible_nominal_total,
    };
    let state_event_digest =
        contributor_state_event_digest(&input.binding, &state_projection, limits)
            .map_err(protocol_error)?;
    let receipt = ContributorReceiptV1 {
        binding: input.binding.clone(),
        contributor_target_precondition: input.precondition.clone(),
        contributor_count: input.contributor_count,
        contributor_root: input.contributor_root,
        eligible_nominal_total: input.eligible_nominal_total,
        state_event_digest,
    };
    receipt
        .validate_projection(&state_projection, limits)
        .map_err(protocol_error)?;
    receipt.receipt_hash(limits).map_err(protocol_error)?;

    let installed = CertifiedContributorGenerationProjection {
        worldwide_day: input.precondition.worldwide_day,
        series_version: next_version,
        contributor_root: input.contributor_root,
        contributor_count: input.contributor_count,
        eligible_nominal_total: input.eligible_nominal_total,
    };
    storage.with_checkpoint(|| {
        intex.ocomp_contributor_root.write(
            &WorldwideDay::new(installed.worldwide_day),
            installed.contributor_root,
        )?;
        intex.ocomp_eligible_nominal_total.write(
            &WorldwideDay::new(installed.worldwide_day),
            installed.eligible_nominal_total,
        )?;
        // Metadata contains the version selector and switches only after the
        // root and total are durable.
        intex.ocomp_contributor_metadata.write(
            &WorldwideDay::new(installed.worldwide_day),
            installed.metadata_word(),
        )?;
        storage.emit_event(
            INTEX_ADDRESS,
            IIntex::CertifiedContributorRootInstalled {
                activationCallId: input.binding.activation_call_id,
                worldwideDay: input.precondition.worldwide_day,
                seriesVersionBefore: input.precondition.expected_series_version,
                seriesVersionAfter: next_version,
                contributorCount: input.contributor_count,
                contributorRoot: input.contributor_root,
                eligibleNominalTotal: input.eligible_nominal_total,
                stateEventDigest: state_event_digest,
            }
            .encode_log_data(),
        )?;
        // As with every owner step, advance the non-journaled cursor only after
        // all journaled state and event writes succeed.
        capability.authorize_contributor_installation()?;
        Ok(())
    })?;

    Ok(receipt)
}

fn validate_input(
    capability: &CertifiedLysisActivation<'_>,
    input: &CertifiedContributorRootV1,
) -> Result<()> {
    if capability.activation_call_id() != input.binding.activation_call_id {
        return Err(revert("certified contributor activation binding mismatch"));
    }
    if input.contributor_root.is_zero() {
        return Err(revert("certified contributor root is zero"));
    }
    if input.contributor_count > input.precondition.max_contributor_count
        || input.eligible_nominal_total > input.precondition.max_eligible_nominal_total
        || (input.contributor_count == 0) != input.eligible_nominal_total.is_zero()
    {
        return Err(revert("certified contributor aggregate exceeds its bound"));
    }
    Ok(())
}

fn protocol_error(error: outbe_ocomp_protocol::ProtocolError) -> PrecompileError {
    PrecompileError::Fatal(format!(
        "invalid certified contributor protocol value: {error}"
    ))
}

fn revert(reason: &'static str) -> PrecompileError {
    PrecompileError::Revert(reason.into())
}

#[cfg(test)]
mod tests;
