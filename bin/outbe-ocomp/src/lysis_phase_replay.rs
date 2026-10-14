//! Supervisor-side semantic replay for frozen LYSIS_V1 phases.

use alloy_primitives::B256;
use outbe_lysis::program_v1::{
    artifacts::{
        decode_amount_run, decode_gratis_prefix_down_output, encode_finalized_output_run,
        GratisPrefixDownOutputV1, LysisArtifactErrorV1,
    },
    phases::output_finalize,
    planner::{LysisPlannerBindingsV1, LysisPlannerV1, PlannerErrorV1, PRIMARY_WORK_SHARD_SIZE},
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    input::{AuthenticatedInputChunkV1, InputChunkRefV1, InputManifestV1},
    profile::ProtocolBundleV1,
    unit::{
        BinaryReducerNode, InputPurpose, InputSourceKind, PlanCommitmentV1, UnitArtifactV1,
        UnitInterval, UnitPhase, UnitSpecV1, WorkOutputHeaderV1,
    },
    CasObjectRefV1, ObjectKind, ProtocolError, SchemaLimits, UnitFinishedStatus, UnitFinishedV1,
};
use thiserror::Error;

use crate::{
    admission_catalog::{
        AdmissionCatalogError, AdmissionOutcome, AdmissionPositionV1, VerifiedAdmissionCatalog,
    },
    cas::{CasError, FilesystemCas, FilesystemCasReader},
    inbox::{WorkerInbox, WorkerInboxError},
    worker::{execute_unit, UnitExecutionAuthority},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedOutputFinalizeUnitV1 {
    pub artifact_ref: CasObjectRefV1,
    pub admission: AdmissionOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedReplayedCoreUnitV1 {
    pub artifact_ref: CasObjectRefV1,
    pub admission: AdmissionOutcome,
}

#[derive(Debug, Error)]
pub enum LysisPhaseReplayError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Planner(#[from] PlannerErrorV1),
    #[error(transparent)]
    LysisArtifact(#[from] LysisArtifactErrorV1),
    #[error("reported OUTPUT_FINALIZE artifact does not equal deterministic semantic replay")]
    ReplayMismatch,
    #[error("OUTPUT_FINALIZE replay authority or producer binding mismatch")]
    BindingMismatch,
    #[error("worker-independent core phase replay failed")]
    ReplayExecutionFailed,
    #[error("phase {0:?} requires its dedicated replay/adoption boundary")]
    DedicatedReplayRequired(UnitPhase),
    #[error(transparent)]
    Inbox(#[from] WorkerInboxError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error(transparent)]
    AdmissionCatalog(#[from] AdmissionCatalogError),
}

/// Re-executes one bounded non-shuffle core phase through the same canonical
/// pure execution path used by the worker and requires byte-identical output.
///
/// Shuffle and ROOT_REDUCE have dedicated descendant/result adoption
/// boundaries. OUTPUT_FINALIZE keeps its stricter typed helper below because
/// its subtotal is an explicit final conservation authority.
pub fn verify_core_phase_replay(
    (plan_ordinal, spec, reported): (u32, &UnitSpecV1, &UnitArtifactV1),
    (plan, manifest): (&PlanCommitmentV1, &InputManifestV1),
    (input_chunks, producer_artifacts): (
        &[(InputChunkRefV1, AuthenticatedInputChunkV1)],
        &[UnitArtifactV1],
    ),
    (bundle, reader, inbox): (&ProtocolBundleV1, &FilesystemCasReader, &WorkerInbox),
    limits: &SchemaLimits,
) -> Result<(), LysisPhaseReplayError> {
    if !is_replayed_core_phase(spec.phase) {
        return Err(LysisPhaseReplayError::DedicatedReplayRequired(spec.phase));
    }
    reported.validate_against(spec, limits)?;
    let expected = execute_unit(
        spec,
        UnitExecutionAuthority {
            plan,
            unit_index: plan_ordinal,
            manifest,
            input_chunks,
            producer_artifacts,
            bundle,
            limits,
            reader,
            inbox,
            cancelled: None,
        },
    )
    .map_err(|_| LysisPhaseReplayError::ReplayExecutionFailed)?;
    if expected.encode_canonical(limits)? != reported.encode_canonical(limits)? {
        return Err(LysisPhaseReplayError::ReplayMismatch);
    }
    Ok(())
}

pub(crate) fn verify_producer_only_phase_replay(
    (expected_phase, invariant): (UnitPhase, &'static str),
    (plan_ordinal, spec, reported): (u32, &UnitSpecV1, &UnitArtifactV1),
    (plan, manifest): (&PlanCommitmentV1, &InputManifestV1),
    (producer_artifacts, bundle): (&[UnitArtifactV1], &ProtocolBundleV1),
    (reader, replay_inbox, limits): (&FilesystemCasReader, &WorkerInbox, &SchemaLimits),
) -> Result<(), LysisPhaseReplayError> {
    if spec.phase != expected_phase {
        return Err(ProtocolError::InvalidInvariant(invariant).into());
    }
    reported.validate_against(spec, limits)?;
    let expected = execute_unit(
        spec,
        UnitExecutionAuthority {
            plan,
            unit_index: plan_ordinal,
            manifest,
            input_chunks: &[],
            producer_artifacts,
            bundle,
            limits,
            reader,
            inbox: replay_inbox,
            cancelled: None,
        },
    )
    .map_err(|_| LysisPhaseReplayError::ReplayExecutionFailed)?;
    if expected.encode_canonical(limits)? != reported.encode_canonical(limits)? {
        return Err(LysisPhaseReplayError::ReplayMismatch);
    }
    Ok(())
}

/// Publishes and admits a worker report only after supervisor-side semantic
/// re-execution of the exact plan-bound phase.
pub fn admit_reported_core_phase_unit(
    (position, spec, finished): (AdmissionPositionV1, &UnitSpecV1, &UnitFinishedV1),
    (plan, manifest): (&PlanCommitmentV1, &InputManifestV1),
    (input_chunks, producer_artifacts): (
        &[(InputChunkRefV1, AuthenticatedInputChunkV1)],
        &[UnitArtifactV1],
    ),
    (bundle, reader, inbox): (&ProtocolBundleV1, &FilesystemCasReader, &WorkerInbox),
    (cas, catalog, limits): (&FilesystemCas, &mut VerifiedAdmissionCatalog, &SchemaLimits),
) -> Result<AdmittedReplayedCoreUnitV1, LysisPhaseReplayError> {
    let unit_id = spec.unit_id(limits)?;
    if finished.status != UnitFinishedStatus::Success || finished.unit_id != unit_id {
        return Err(LysisPhaseReplayError::BindingMismatch);
    }
    let reported = inbox.read_reported(
        finished.unit_id,
        finished.exact_staged_bytes,
        finished.transport_digest,
    )?;
    let artifact = UnitArtifactV1::decode_canonical(reported.bytes(), limits)?;
    verify_core_phase_replay(
        (position.plan_ordinal, spec, &artifact),
        (plan, manifest),
        (input_chunks, producer_artifacts),
        (bundle, reader, inbox),
        limits,
    )?;
    let (artifact_ref, admission) =
        publish_and_admit_unit_artifact(position, spec, finished, reported.bytes(), (cas, catalog))?;
    Ok(AdmittedReplayedCoreUnitV1 {
        artifact_ref,
        admission,
    })
}

/// Makes one reported OUTPUT_FINALIZE unit authoritative only after exact
/// semantic replay from its already-admitted producer artifacts.
pub fn admit_reported_output_finalize_unit(
    (position, shard_ordinal): (AdmissionPositionV1, u32),
    (spec, finished): (&UnitSpecV1, &UnitFinishedV1),
    (amount_artifact, prefix_artifact): (&UnitArtifactV1, &UnitArtifactV1),
    (plan, manifest, bundle): (&PlanCommitmentV1, &InputManifestV1, &ProtocolBundleV1),
    (inbox, cas, catalog, limits): (
        &WorkerInbox,
        &FilesystemCas,
        &mut VerifiedAdmissionCatalog,
        &SchemaLimits,
    ),
) -> Result<AdmittedOutputFinalizeUnitV1, LysisPhaseReplayError> {
    spec.validate_semantics(limits)?;
    let unit_id = spec.unit_id(limits)?;
    if finished.status != UnitFinishedStatus::Success || finished.unit_id != unit_id {
        return Err(LysisPhaseReplayError::BindingMismatch);
    }
    let reported = inbox.read_reported(
        finished.unit_id,
        finished.exact_staged_bytes,
        finished.transport_digest,
    )?;
    let artifact = UnitArtifactV1::decode_canonical(reported.bytes(), limits)?;
    verify_output_finalize_replay(
        (shard_ordinal, spec, &artifact),
        (amount_artifact, prefix_artifact),
        (plan, manifest, bundle),
        limits,
    )?;
    let (artifact_ref, admission) =
        publish_and_admit_unit_artifact(position, spec, finished, reported.bytes(), (cas, catalog))?;
    Ok(AdmittedOutputFinalizeUnitV1 {
        artifact_ref,
        admission,
    })
}

/// Re-executes one OUTPUT_FINALIZE unit from its exact already-verified
/// producers and requires byte-identical equality with the worker report.
///
/// Producer artifacts are an admission precondition: their own phase replay
/// must complete before the caller invokes this verifier.
pub fn verify_output_finalize_replay(
    (shard_ordinal, spec, reported): (u32, &UnitSpecV1, &UnitArtifactV1),
    (amount_artifact, prefix_artifact): (&UnitArtifactV1, &UnitArtifactV1),
    (plan, manifest, bundle): (&PlanCommitmentV1, &InputManifestV1, &ProtocolBundleV1),
    limits: &SchemaLimits,
) -> Result<(), LysisPhaseReplayError> {
    reported.validate_against(spec, limits)?;
    let expected = replay_output_finalize_artifact(
        shard_ordinal,
        spec,
        (amount_artifact, prefix_artifact),
        (plan, manifest, bundle),
        limits,
    )?;
    if expected != *reported {
        return Err(LysisPhaseReplayError::ReplayMismatch);
    }
    Ok(())
}

/// Produces the one canonical OUTPUT_FINALIZE artifact used identically by
/// workers and by supervisor replay.
pub fn replay_output_finalize_artifact(
    shard_ordinal: u32,
    spec: &UnitSpecV1,
    (amount_artifact, prefix_artifact): (&UnitArtifactV1, &UnitArtifactV1),
    (plan, manifest, bundle): (&PlanCommitmentV1, &InputManifestV1, &ProtocolBundleV1),
    limits: &SchemaLimits,
) -> Result<UnitArtifactV1, LysisPhaseReplayError> {
    validate_output_finalize_authority(shard_ordinal, spec, (plan, manifest, bundle), limits)?;
    let amount_unit_id = exact_unit_output_source(spec, InputPurpose::AmountRecords)?;
    let prefix_unit_id = exact_unit_output_source(spec, InputPurpose::GratisPrefixTable)?;
    let manifest_encoded_bytes =
        u64::try_from(manifest.encode_canonical(limits)?.len()).map_err(|_| {
            ProtocolError::IntegerOverflow {
                what: "OUTPUT_FINALIZE replay manifest bytes",
            }
        })?;
    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: plan.protocol_bundle_hash,
        job_id: plan.job_id,
        attempt: plan.attempt,
        input_manifest_hash: plan.input_manifest_hash,
        input_manifest_encoded_bytes: manifest_encoded_bytes,
        fidelity_opening_root: manifest.fidelity_opening_root,
        oracle_opening_root: manifest.oracle_opening_root,
        wwd: plan.wwd,
        lysis_limit_minor: plan.lysis_limit_minor,
        logical_evaluation_time: plan.logical_evaluation_time,
        tribute_count: plan.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: plan.planner_spec_version,
        reducer_spec_version: plan.reducer_spec_version,
    })?;
    planner.validate_output_finalize_unit(
        shard_ordinal,
        spec,
        amount_unit_id,
        prefix_unit_id,
        limits,
    )?;

    let mut amount_interval_binding = spec.clone();
    amount_interval_binding.phase = UnitPhase::AmountMap;
    validate_producer_artifact_binding(
        amount_artifact,
        (amount_unit_id, UnitPhase::AmountMap),
        (spec, &amount_interval_binding),
        limits,
    )?;

    let mut prefix_interval_binding = spec.clone();
    prefix_interval_binding.phase = UnitPhase::GratisPrefixDown;
    prefix_interval_binding.interval = UnitInterval::BinaryReducerNode(BinaryReducerNode {
        level: 0,
        index: shard_ordinal,
    });
    validate_producer_artifact_binding(
        prefix_artifact,
        (prefix_unit_id, UnitPhase::GratisPrefixDown),
        (spec, &prefix_interval_binding),
        limits,
    )?;

    let amount = decode_amount_run(amount_artifact.phase_payload(limits)?, limits)?;
    let amount_header = amount_artifact.output_header(limits)?;
    let GratisPrefixDownOutputV1::Leaf(prefix) =
        decode_gratis_prefix_down_output(prefix_artifact.phase_payload(limits)?, limits)?
    else {
        return Err(LysisPhaseReplayError::BindingMismatch);
    };
    let prefix_header = prefix_artifact.output_header(limits)?;
    if !matches_output_finalize_coverage(shard_ordinal, &amount, &amount_header, &prefix_header)? {
        return Err(LysisPhaseReplayError::BindingMismatch);
    }

    let output = output_finalize(&amount, &prefix).map_err(LysisArtifactErrorV1::from)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: amount_header.output_coverage_root,
            output_coverage_root: amount_header.output_coverage_root,
            source_coverage_count: amount_header.output_coverage_count,
            output_coverage_count: amount_header.output_coverage_count,
        },
        BoundedBytes(encode_finalized_output_run(&output, limits)?),
        limits,
    )
    .map_err(LysisPhaseReplayError::from)
}

const fn is_replayed_core_phase(phase: UnitPhase) -> bool {
    matches!(
        phase,
        UnitPhase::Enumerate
            | UnitPhase::FidelityMap
            | UnitPhase::FixedReduce
            | UnitPhase::AmountMap
            | UnitPhase::GratisPrefix
            | UnitPhase::GratisPrefixDown
    )
}

fn publish_and_admit_unit_artifact(
    position: AdmissionPositionV1,
    spec: &UnitSpecV1,
    finished: &UnitFinishedV1,
    reported_bytes: &[u8],
    (cas, catalog): (&FilesystemCas, &mut VerifiedAdmissionCatalog),
) -> Result<(CasObjectRefV1, AdmissionOutcome), LysisPhaseReplayError> {
    let mut artifact_ref = cas.publish_bytes(reported_bytes)?;
    if artifact_ref.transport_digest != finished.transport_digest
        || artifact_ref.encoded_bytes != finished.exact_staged_bytes
    {
        return Err(LysisPhaseReplayError::BindingMismatch);
    }
    artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    cas.read_verified(&artifact_ref)?;
    let admission = catalog.admit_verified_unit(position, spec, artifact_ref.clone(), None)?;
    Ok((artifact_ref, admission))
}

fn validate_output_finalize_authority(
    shard_ordinal: u32,
    spec: &UnitSpecV1,
    (plan, manifest, bundle): (&PlanCommitmentV1, &InputManifestV1, &ProtocolBundleV1),
    limits: &SchemaLimits,
) -> Result<(), LysisPhaseReplayError> {
    plan.validate_semantics()?;
    manifest.validate_semantics(limits)?;
    spec.validate_semantics(limits)?;
    let bundle_hash = bundle.protocol_bundle_hash(limits)?;
    let matches_manifest = plan.protocol_bundle_hash == bundle_hash
        && plan.input_manifest_hash == manifest.manifest_hash(limits)?
        && plan.tribute_count == manifest.tribute_count;
    let matches_spec =
        shard_ordinal < plan.primary_work_unit_count && spec.phase == UnitPhase::OutputFinalize;
    if !matches_manifest || !matches_spec {
        return Err(LysisPhaseReplayError::BindingMismatch);
    }
    Ok(())
}

fn validate_producer_artifact_binding(
    artifact: &UnitArtifactV1,
    (expected_unit_id, expected_phase): (B256, UnitPhase),
    (consumer_spec, interval_binding): (&UnitSpecV1, &UnitSpecV1),
    limits: &SchemaLimits,
) -> Result<(), LysisPhaseReplayError> {
    artifact.validate_semantics(limits)?;
    let matches_identity = artifact.unit_id == expected_unit_id
        && artifact.protocol_bundle_hash == consumer_spec.protocol_bundle_hash
        && artifact.job_id == consumer_spec.job_id
        && artifact.attempt == consumer_spec.attempt;
    let matches_shape = artifact.phase == expected_phase
        && artifact.interval_commitment == interval_binding.interval_commitment(limits)?;
    if !matches_identity || !matches_shape {
        return Err(LysisPhaseReplayError::BindingMismatch);
    }
    Ok(())
}

fn matches_output_finalize_coverage(
    shard_ordinal: u32,
    amount: &outbe_lysis::program_v1::phases::AmountRunV1,
    amount_header: &WorkOutputHeaderV1,
    prefix_header: &WorkOutputHeaderV1,
) -> Result<bool, LysisPhaseReplayError> {
    Ok(amount.start_ordinal / PRIMARY_WORK_SHARD_SIZE == shard_ordinal
        && amount.coverage_root()? == amount_header.output_coverage_root
        && amount_header.output_coverage_root == prefix_header.output_coverage_root
        && amount_header.output_coverage_count == prefix_header.output_coverage_count)
}

fn exact_unit_output_source(
    spec: &UnitSpecV1,
    purpose: InputPurpose,
) -> Result<B256, LysisPhaseReplayError> {
    let mut matches = spec.canonical_ordered_inputs.iter().filter(|input| {
        input.purpose == purpose
            && input.source_kind == InputSourceKind::UnitOutput
            && !input.source_id.is_zero()
    });
    let source = matches
        .next()
        .ok_or(LysisPhaseReplayError::BindingMismatch)?
        .source_id;
    if matches.next().is_some() {
        return Err(LysisPhaseReplayError::BindingMismatch);
    }
    Ok(source)
}
