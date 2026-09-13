use super::execute_amount_map_unit;
use super::execute_enumerate_unit;
use super::execute_fidelity_map_unit;
use super::execute_fixed_reduce_unit;
use super::execute_gratis_prefix_down_unit;
use super::execute_gratis_prefix_unit;
use super::execute_output_finalize_unit;
use super::execute_root_reduce_unit;
use super::execute_shuffle_unit;
use super::require_authenticated_input;
use super::require_plan_binding;
use super::ExpectedPlanBindingsV1;
use super::UnitExecutionAuthority;
use super::WorkerConfig;
use super::WorkerError;

use crate::cas::FilesystemCasReader;
use crate::control::poc_schema_limits;

use crate::inbox::WorkerInbox;

use crate::input_artifacts::decode_verified_input_chunk;
use crate::input_artifacts::derive_input_chunk_ref;

use alloy_primitives::B256;

use outbe_ocomp_protocol::input::InputManifestV1;

use outbe_ocomp_protocol::unit::InputPurpose;

use outbe_ocomp_protocol::unit::PlanCommitmentV1;
use outbe_ocomp_protocol::unit::UnitArtifactV1;

use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;

use outbe_ocomp_protocol::verify_ordered_list_membership;
use outbe_ocomp_protocol::ListKind;
use outbe_ocomp_protocol::ObjectKind;
use outbe_ocomp_protocol::RunUnitV1;

use outbe_ocomp_protocol::UnitFinishedStatus;
use outbe_ocomp_protocol::UnitFinishedV1;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

pub(super) fn require_lease_active(cancelled: Option<&AtomicBool>) -> Result<(), WorkerError> {
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        Err(WorkerError::LeaseCancelled)
    } else {
        Ok(())
    }
}

pub(super) fn execute_claimed_unit(
    config: &WorkerConfig,
    request: &RunUnitV1,
    cancelled: Option<&AtomicBool>,
) -> Result<UnitFinishedV1, WorkerError> {
    require_lease_active(cancelled)?;
    let reader = FilesystemCasReader::open(&config.cas_root, config.cas_limits)?;
    let inbox = WorkerInbox::open(&config.inbox_root, config.inbox_limits)?;
    let limits = poc_schema_limits();
    if request.protocol_bundle_hash != config.identity.protocol_bundle_hash {
        return Err(WorkerError::UnitBindingMismatch);
    }
    if request.plan_hash.is_zero() {
        return Err(WorkerError::ZeroPlanHash);
    }
    let spec = UnitSpecV1::decode_canonical(&request.canonical_unit_spec.0, &limits)?;
    if spec.protocol_bundle_hash != request.protocol_bundle_hash
        || spec.job_id != request.job_id
        || spec.attempt != request.attempt
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let unit_id = spec.unit_id(&limits)?;
    let plan_object = reader.read_verified(&request.plan_ref)?;
    let plan = PlanCommitmentV1::decode_canonical_record(plan_object.bytes(), &limits)?;
    let manifest_object = reader.read_verified(&request.input_manifest_ref)?;
    let manifest = InputManifestV1::decode_canonical(manifest_object.bytes(), &limits)?;
    manifest.validate_against_bundle(config.protocol_bundle.bundle(), &limits)?;
    if manifest.job_id != spec.job_id || manifest.attempt != spec.attempt {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let manifest_hash = manifest.manifest_hash(&limits)?;
    require_plan_binding(
        &plan,
        ExpectedPlanBindingsV1 {
            plan_hash: request.plan_hash,
            protocol_bundle_hash: request.protocol_bundle_hash,
            job_id: request.job_id,
            attempt: request.attempt,
            input_manifest_hash: manifest_hash,
            wwd: manifest.wwd,
            tribute_count: manifest.tribute_count,
            planner_spec_version: spec.planner_spec_version,
            reducer_spec_version: spec.reducer_spec_version,
        },
        &limits,
    )?;
    if spec.phase == UnitPhase::Enumerate {
        verify_ordered_list_membership(
            ListKind::UnitSpecificationsArtifacts,
            plan.primary_work_unit_count,
            request.unit_index,
            &request.canonical_unit_spec.0,
            &request.unit_membership_siblings,
            plan.primary_work_unit_root,
        )?;
    } else if !request.unit_membership_siblings.is_empty() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    require_authenticated_input(
        &spec,
        InputPurpose::InputManifest,
        manifest_hash,
        1,
        request.input_manifest_ref.encoded_bytes,
    )?;
    let mut input_chunks = Vec::new();
    let mut producer_artifacts = Vec::new();
    for reference in &request.ordered_input_refs {
        require_lease_active(cancelled)?;
        let object = reader.read_verified(reference)?;
        match reference.expected_ocb1_kind {
            Some(kind) if kind == ObjectKind::AuthenticatedInputChunkV1.tag() => {
                let derived =
                    derive_input_chunk_ref(&object, config.protocol_bundle.bundle(), &limits)?
                        .reference;
                let chunk =
                    decode_verified_input_chunk(&object, config.protocol_bundle.bundle(), &limits)?;
                if chunk.job_id != spec.job_id
                    || chunk.protocol_bundle_hash != spec.protocol_bundle_hash
                {
                    return Err(WorkerError::UnitBindingMismatch);
                }
                input_chunks.push((derived, chunk));
            }
            Some(kind) if kind == ObjectKind::UnitArtifactV1.tag() => {
                producer_artifacts.push(UnitArtifactV1::decode_canonical(object.bytes(), &limits)?);
            }
            _ => return Err(WorkerError::UnitBindingMismatch),
        }
    }

    require_lease_active(cancelled)?;
    let finished = match execute_unit(
        &spec,
        UnitExecutionAuthority {
            plan: &plan,
            unit_index: request.unit_index,
            manifest: &manifest,
            input_chunks: &input_chunks,
            producer_artifacts: &producer_artifacts,
            bundle: config.protocol_bundle.bundle(),
            limits: &limits,
            reader: &reader,
            inbox: &inbox,
            cancelled,
        },
    )
    .and_then(|artifact| {
        require_lease_active(cancelled)?;
        artifact
            .encode_canonical(&limits)
            .map_err(WorkerError::from)
    })
    .and_then(|bytes| {
        require_lease_active(cancelled)?;
        inbox.adopt(unit_id, &bytes).map_err(WorkerError::from)
    }) {
        Ok(staged) => {
            let reference = staged.reference();
            UnitFinishedV1 {
                unit_id,
                status: UnitFinishedStatus::Success,
                exact_staged_bytes: reference.encoded_bytes,
                transport_digest: reference.transport_digest,
            }
        }
        Err(error) => {
            eprintln!(
                "OCOMP worker unit failed: unit={unit_id:#x} phase={:?} index={} error={error}",
                spec.phase, request.unit_index
            );
            UnitFinishedV1 {
                unit_id,
                status: UnitFinishedStatus::Failed,
                exact_staged_bytes: 0,
                transport_digest: B256::ZERO,
            }
        }
    };
    Ok(finished)
}

pub(crate) fn execute_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    require_lease_active(authority.cancelled)?;
    match spec.phase {
        UnitPhase::Enumerate => execute_enumerate_unit(
            spec,
            authority.manifest,
            authority.input_chunks,
            authority.producer_artifacts,
            authority.limits,
            authority.cancelled,
        ),
        UnitPhase::FidelityMap => execute_fidelity_map_unit(spec, authority),
        UnitPhase::FixedReduce => execute_fixed_reduce_unit(spec, authority),
        UnitPhase::AmountMap => execute_amount_map_unit(spec, authority),
        UnitPhase::GratisPrefix => execute_gratis_prefix_unit(spec, authority),
        UnitPhase::GratisPrefixDown => execute_gratis_prefix_down_unit(spec, authority),
        UnitPhase::OutputFinalize => execute_output_finalize_unit(spec, authority),
        UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle => execute_shuffle_unit(spec, authority),
        UnitPhase::RootReduce => execute_root_reduce_unit(spec, authority),
    }
}
