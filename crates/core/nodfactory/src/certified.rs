//! Certified OCOMP installation boundary for Nod generations.
//!
//! This path installs only constant-size root authority and metadata. It never
//! decodes or iterates `NodActionV1`, and it has no public precompile selector.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_nod::{NodCertifiedGenerationProjection, NodContract};
use outbe_ocomp_protocol::{
    intent::NodTargetPreconditionV1,
    receipts::{
        nod_state_event_digest, EffectBindingV1, NodBatchReceiptV1, NodStateEventProjectionV1,
    },
    result::{ExactCountsV1, ResultRootsV1},
    SchemaLimits,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::NOD_FACTORY_ADDRESS,
    error::{PrecompileError, Result},
    storage::{CertifiedLysisActivation, StorageHandle},
};

use crate::precompile::INodFactory;

/// Closed, constant-size Nod owner input derived from a verified Lysis apply
/// plan. The raw values carry no authority. Installation additionally requires
/// the runtime-only [`CertifiedLysisActivation`] capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedNodGenerationV1 {
    pub binding: EffectBindingV1,
    pub program_semantics_hash: alloy_primitives::B256,
    pub precondition: NodTargetPreconditionV1,
    pub roots: ResultRootsV1,
    pub counts: ExactCountsV1,
    pub nod_amount_total: U256,
    pub lysis_allocation_minor: U256,
    pub issued_at: u64,
}

/// Compare-and-set one certified per-WWD Nod generation.
///
/// The method is Rust-visible for the Metadosis activation coordinator, but is
/// unreachable from ABI dispatch and cannot succeed without the private
/// execution-frame capability.
pub fn install_certified_generation(
    storage: &StorageHandle<'_>,
    capability: &mut CertifiedLysisActivation<'_>,
    input: &CertifiedNodGenerationV1,
    limits: &SchemaLimits,
) -> Result<NodBatchReceiptV1> {
    validate_input(capability, input)?;

    let worldwide_day = WorldwideDay::new(input.precondition.wwd);
    let nod = NodContract::new(storage.clone());
    let next_generation = input
        .precondition
        .target_generation
        .checked_add(1)
        .ok_or_else(|| revert("certified Nod generation overflow"))?;
    let state_projection = NodStateEventProjectionV1 {
        wwd: input.precondition.wwd,
        target_generation: input.precondition.target_generation,
        namespace_root_before: input.precondition.namespace_root_before,
        nod_count: input.counts.nod_count,
        nod_root: input.roots.nod_root,
        nod_amount_total: input.nod_amount_total,
        lysis_allocation_minor: input.lysis_allocation_minor,
        issued_at: input.issued_at,
    };
    let state_event_digest = nod_state_event_digest(&input.binding, &state_projection, limits)
        .map_err(protocol_error)?;
    let receipt = NodBatchReceiptV1 {
        binding: input.binding.clone(),
        nod_target_precondition: input.precondition.clone(),
        nod_count: input.counts.nod_count,
        nod_root: input.roots.nod_root,
        nod_amount_total: input.nod_amount_total,
        lysis_allocation_minor: input.lysis_allocation_minor,
        issued_at: input.issued_at,
        state_event_digest,
    };
    receipt
        .validate_projection(&state_projection, limits)
        .map_err(protocol_error)?;
    receipt.receipt_hash(limits).map_err(protocol_error)?;

    if let Some(existing) = nod.ocomp_certified_generation(worldwide_day)? {
        if generation_matches(&existing, input, next_generation) {
            capability.authorize_nod_installation()?;
            return Ok(receipt);
        }
        return Err(revert(
            "a different certified Nod generation already exists for this WWD",
        ));
    }
    if input.precondition.target_generation != 0
        || !input.precondition.namespace_root_before.is_zero()
    {
        return Err(revert("certified Nod target precondition changed"));
    }

    let raw_head = nod.ocomp_materialization_head_sequence.read()?;
    let raw_tail = nod.ocomp_materialization_tail_sequence.read()?;
    let tail_sequence = match (raw_head, raw_tail) {
        (head, tail) if head != 0 && tail != 0 && head <= tail => {
            if head < tail {
                nod.ocomp_materialization_head()?;
            }
            tail
        }
        _ => {
            return Err(PrecompileError::Fatal(
                "Nod materialization FIFO bounds are malformed".into(),
            ))
        }
    };
    let queued_wwd = nod.ocomp_materialization_queue_wwd.read(&tail_sequence)?;
    if queued_wwd.value() != 0 {
        return Err(PrecompileError::Fatal(
            "Nod materialization FIFO tail entry is occupied".into(),
        ));
    }
    let next_tail = tail_sequence
        .checked_add(1)
        .ok_or_else(|| PrecompileError::Fatal("Nod materialization FIFO tail overflow".into()))?;
    let activation_height = storage.block_number()?;

    let installed = NodCertifiedGenerationProjection {
        worldwide_day,
        generation: next_generation,
        job_id: input.binding.job_id,
        protocol_bundle_hash: input.binding.protocol_bundle_hash,
        program_semantics_hash: input.program_semantics_hash,
        nod_root: input.roots.nod_root,
        bucket_root: input.roots.bucket_root,
        output_manifest_root: input.roots.output_manifest_root,
        tribute_count: input.counts.tribute_count,
        nod_count: input.counts.nod_count,
        bucket_count: input.counts.bucket_count,
        nod_amount_total: input.nod_amount_total,
        lysis_allocation_minor: input.lysis_allocation_minor,
        issued_at: input.issued_at,
        next_nod_ordinal: 0,
        last_progress_height: activation_height,
    };

    storage.with_checkpoint(|| {
        nod.ocomp_namespace_root
            .write(&worldwide_day, installed.nod_root)?;
        nod.ocomp_bucket_root
            .write(&worldwide_day, installed.bucket_root)?;
        nod.ocomp_output_manifest_root
            .write(&worldwide_day, installed.output_manifest_root)?;
        nod.ocomp_generation_metadata
            .write(&worldwide_day, installed.metadata_word())?;
        nod.ocomp_nod_amount_total
            .write(&worldwide_day, installed.nod_amount_total)?;
        nod.ocomp_lysis_allocation_minor
            .write(&worldwide_day, installed.lysis_allocation_minor)?;
        nod.ocomp_materialization_job_id
            .write(&worldwide_day, installed.job_id)?;
        nod.ocomp_materialization_protocol_bundle_hash
            .write(&worldwide_day, installed.protocol_bundle_hash)?;
        nod.ocomp_materialization_program_semantics_hash
            .write(&worldwide_day, installed.program_semantics_hash)?;
        nod.ocomp_materialization_next_nod_ordinal
            .write(&worldwide_day, installed.next_nod_ordinal)?;
        nod.ocomp_materialization_last_progress_height
            .write(&worldwide_day, installed.last_progress_height)?;
        nod.ocomp_materialization_queue_wwd
            .write(&tail_sequence, worldwide_day)?;
        nod.ocomp_materialization_tail_sequence.write(next_tail)?;
        // The generation is the active-state selector and therefore switches
        // only after every root and scalar write succeeds.
        nod.ocomp_target_generation
            .write(&worldwide_day, installed.generation)?;
        storage.emit_event(
            NOD_FACTORY_ADDRESS,
            INodFactory::CertifiedNodGenerationInstalled {
                activationCallId: input.binding.activation_call_id,
                worldwideDay: input.precondition.wwd,
                targetGeneration: input.precondition.target_generation,
                namespaceRootBefore: input.precondition.namespace_root_before,
                tributeCount: input.counts.tribute_count,
                nodCount: input.counts.nod_count,
                bucketCount: input.counts.bucket_count,
                nodRoot: input.roots.nod_root,
                bucketRoot: input.roots.bucket_root,
                outputManifestRoot: input.roots.output_manifest_root,
                totalSettlementCostMinor: input.nod_amount_total,
                lysisAllocationMinor: input.lysis_allocation_minor,
                issuedAt: input.issued_at,
                stateEventDigest: state_event_digest,
            }
            .encode_log_data(),
        )?;
        // The capability cursor is not journaled storage. Advance it only
        // after every owner write and event succeeds, so a caught
        // mutation failure cannot skip the Nod owner step.
        capability.authorize_nod_installation()?;
        Ok(())
    })?;

    Ok(receipt)
}

fn generation_matches(
    existing: &NodCertifiedGenerationProjection,
    input: &CertifiedNodGenerationV1,
    expected_generation: u64,
) -> bool {
    existing.generation == expected_generation
        && binding_matches(existing, input)
        && roots_match(existing, input)
        && totals_match(existing, input)
}

fn binding_matches(
    existing: &NodCertifiedGenerationProjection,
    input: &CertifiedNodGenerationV1,
) -> bool {
    existing.job_id == input.binding.job_id
        && existing.program_semantics_hash == input.program_semantics_hash
}

fn roots_match(
    existing: &NodCertifiedGenerationProjection,
    input: &CertifiedNodGenerationV1,
) -> bool {
    existing.nod_root == input.roots.nod_root
        && existing.bucket_root == input.roots.bucket_root
        && existing.output_manifest_root == input.roots.output_manifest_root
}

fn totals_match(
    existing: &NodCertifiedGenerationProjection,
    input: &CertifiedNodGenerationV1,
) -> bool {
    counts_match(existing, input)
        && existing.nod_amount_total == input.nod_amount_total
        && existing.lysis_allocation_minor == input.lysis_allocation_minor
        && existing.issued_at == input.issued_at
}

fn counts_match(
    existing: &NodCertifiedGenerationProjection,
    input: &CertifiedNodGenerationV1,
) -> bool {
    existing.tribute_count == input.counts.tribute_count
        && existing.nod_count == input.counts.nod_count
        && existing.bucket_count == input.counts.bucket_count
}

fn validate_input(
    capability: &CertifiedLysisActivation<'_>,
    input: &CertifiedNodGenerationV1,
) -> Result<()> {
    if capability.activation_call_id() != input.binding.activation_call_id {
        return Err(revert("certified Nod activation binding mismatch"));
    }
    if input.issued_at == 0 {
        return Err(revert("certified Nod logical issuance time is zero"));
    }
    if input.binding.job_id.is_zero()
        || input.binding.protocol_bundle_hash.is_zero()
        || input.program_semantics_hash.is_zero()
    {
        return Err(revert("certified Nod materialization binding is zero"));
    }
    if input.roots.nod_root.is_zero()
        || input.roots.bucket_root.is_zero()
        || input.roots.output_manifest_root.is_zero()
    {
        return Err(revert("certified Nod generation contains a zero root"));
    }
    if input.counts.tribute_count == 0
        || input.counts.nod_count != input.counts.tribute_count
        || input.counts.nod_count != input.precondition.max_nod_count
        || input.counts.bucket_count > input.counts.nod_count
    {
        return Err(revert("certified Nod generation count mismatch"));
    }
    Ok(())
}

fn protocol_error(error: outbe_ocomp_protocol::ProtocolError) -> PrecompileError {
    PrecompileError::Fatal(format!("invalid certified Nod protocol value: {error}"))
}

fn revert(reason: &'static str) -> PrecompileError {
    PrecompileError::Revert(reason.into())
}

#[cfg(test)]
mod tests;
