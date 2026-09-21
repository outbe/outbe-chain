use super::WorkerError;

use crate::cas::FilesystemCasReader;

use crate::inbox::WorkerInbox;

use alloy_primitives::B256;

use outbe_lysis::program_v1::planner::LysisPlannerBindingsV1;
use outbe_lysis::program_v1::planner::LysisPlannerV1;
use outbe_lysis::program_v1::planner::PlannedProducerV1;
use outbe_lysis::program_v1::planner::PlannedUnitPositionV1;

use outbe_ocomp_protocol::input::AuthenticatedInputChunkV1;

use outbe_ocomp_protocol::input::InputChunkRefV1;
use outbe_ocomp_protocol::input::InputManifestV1;

use outbe_ocomp_protocol::unit::BinaryReducerNode;
use outbe_ocomp_protocol::unit::CanonicalInputRefV1;

use outbe_ocomp_protocol::unit::InputPurpose;
use outbe_ocomp_protocol::unit::InputSourceKind;
use outbe_ocomp_protocol::unit::PlanCommitmentV1;
use outbe_ocomp_protocol::unit::UnitArtifactV1;
use outbe_ocomp_protocol::unit::UnitInterval;

use outbe_ocomp_protocol::unit::UnitSpecV1;

use outbe_ocomp_protocol::SchemaLimits;

use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExpectedPlanBindingsV1 {
    pub(super) plan_hash: B256,
    pub(super) protocol_bundle_hash: B256,
    pub(super) job_id: B256,
    pub(super) attempt: u32,
    pub(super) input_manifest_hash: B256,
    pub(super) wwd: u32,
    pub(super) tribute_count: u32,
    pub(super) planner_spec_version: u16,
    pub(super) reducer_spec_version: u16,
}

pub(crate) struct UnitExecutionAuthority<'a> {
    pub(crate) plan: &'a PlanCommitmentV1,
    pub(crate) unit_index: u32,
    pub(crate) manifest: &'a InputManifestV1,
    pub(crate) input_chunks: &'a [(InputChunkRefV1, AuthenticatedInputChunkV1)],
    pub(crate) producer_artifacts: &'a [UnitArtifactV1],
    pub(crate) bundle: &'a outbe_ocomp_protocol::profile::ProtocolBundleV1,
    pub(crate) limits: &'a SchemaLimits,
    pub(crate) reader: &'a FilesystemCasReader,
    pub(crate) inbox: &'a WorkerInbox,
    pub(crate) cancelled: Option<&'a AtomicBool>,
}

pub(super) fn scan_producer_inputs(
    spec: &UnitSpecV1,
    purpose: InputPurpose,
) -> Result<Vec<&CanonicalInputRefV1>, WorkerError> {
    if spec
        .canonical_ordered_inputs
        .first()
        .map(|input| input.purpose)
        != Some(InputPurpose::InputManifest)
        || spec
            .canonical_ordered_inputs
            .iter()
            .skip(1)
            .any(|input| input.purpose != purpose)
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(spec.canonical_ordered_inputs.iter().skip(1).collect())
}

pub(super) fn unit_or_empty_id(input: &CanonicalInputRefV1) -> Result<Option<B256>, WorkerError> {
    match input.source_kind {
        InputSourceKind::UnitOutput if !input.source_id.is_zero() => Ok(Some(input.source_id)),
        InputSourceKind::CanonicalEmpty => Ok(None),
        _ => Err(WorkerError::UnitBindingMismatch),
    }
}

pub(super) fn resolve_scan_artifacts<'a>(
    consumer: &UnitSpecV1,
    expected: &[PlannedProducerV1],
    inputs: &[&CanonicalInputRefV1],
    artifacts: &'a [UnitArtifactV1],
    limits: &SchemaLimits,
) -> Result<Vec<Option<&'a UnitArtifactV1>>, WorkerError> {
    if expected.len() != inputs.len() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let mut artifacts = artifacts.iter();
    let mut resolved = Vec::with_capacity(expected.len());
    for (producer, input) in expected.iter().zip(inputs) {
        match producer {
            PlannedProducerV1::CanonicalEmpty { .. } => {
                if input.source_kind != InputSourceKind::CanonicalEmpty {
                    return Err(WorkerError::UnitBindingMismatch);
                }
                resolved.push(None);
            }
            PlannedProducerV1::Unit(position) => {
                let artifact = artifacts.next().ok_or(WorkerError::UnitBindingMismatch)?;
                validate_scan_artifact(consumer, *position, input.source_id, artifact, limits)?;
                resolved.push(Some(artifact));
            }
        }
    }
    if artifacts.next().is_some() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(resolved)
}

pub(super) fn validate_scan_artifact(
    consumer: &UnitSpecV1,
    position: PlannedUnitPositionV1,
    expected_unit_id: B256,
    artifact: &UnitArtifactV1,
    limits: &SchemaLimits,
) -> Result<(), WorkerError> {
    artifact.validate_semantics(limits)?;
    if expected_unit_id.is_zero()
        || artifact.unit_id != expected_unit_id
        || artifact.protocol_bundle_hash != consumer.protocol_bundle_hash
        || artifact.job_id != consumer.job_id
        || artifact.attempt != consumer.attempt
        || artifact.phase != position.phase()
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    if let PlannedUnitPositionV1::TreeNode {
        phase,
        level,
        index,
    } = position
    {
        let mut interval_binding = consumer.clone();
        interval_binding.phase = phase;
        interval_binding.interval =
            UnitInterval::BinaryReducerNode(BinaryReducerNode { level, index });
        if artifact.interval_commitment != interval_binding.interval_commitment(limits)? {
            return Err(WorkerError::UnitBindingMismatch);
        }
    } else if let PlannedUnitPositionV1::RunSpan {
        phase,
        start_run,
        end_run,
        ..
    } = position
    {
        let mut interval_binding = consumer.clone();
        interval_binding.phase = phase;
        interval_binding.interval =
            UnitInterval::CanonicalRunSpan(outbe_ocomp_protocol::unit::CanonicalRunSpan {
                start_run,
                end_run,
            });
        if artifact.interval_commitment != interval_binding.interval_commitment(limits)? {
            return Err(WorkerError::UnitBindingMismatch);
        }
    }
    Ok(())
}

pub(super) fn planner_from_authority(
    plan: &PlanCommitmentV1,
    manifest: &InputManifestV1,
    bundle: &outbe_ocomp_protocol::profile::ProtocolBundleV1,
    limits: &SchemaLimits,
) -> Result<LysisPlannerV1, WorkerError> {
    LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: plan.protocol_bundle_hash,
        job_id: plan.job_id,
        attempt: plan.attempt,
        input_manifest_hash: plan.input_manifest_hash,
        input_manifest_encoded_bytes: u64::try_from(manifest.encode_canonical(limits)?.len())
            .map_err(|_| WorkerError::UnitBindingMismatch)?,
        fidelity_opening_root: manifest.fidelity_opening_root,
        oracle_opening_root: manifest.oracle_opening_root,
        wwd: plan.wwd,
        lysis_limit_minor: plan.lysis_limit_minor,
        logical_evaluation_time: plan.logical_evaluation_time,
        tribute_count: plan.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: plan.planner_spec_version,
        reducer_spec_version: plan.reducer_spec_version,
    })
    .map_err(WorkerError::from)
}

pub(super) fn exact_unit_output_source(
    spec: &UnitSpecV1,
    purpose: InputPurpose,
) -> Result<B256, WorkerError> {
    let mut matches = spec.canonical_ordered_inputs.iter().filter(|input| {
        input.purpose == purpose && input.source_kind == InputSourceKind::UnitOutput
    });
    let source = matches
        .next()
        .ok_or(WorkerError::UnitBindingMismatch)?
        .source_id;
    if source.is_zero() || matches.next().is_some() {
        Err(WorkerError::UnitBindingMismatch)
    } else {
        Ok(source)
    }
}

pub(super) fn require_plan_binding(
    plan: &PlanCommitmentV1,
    expected: ExpectedPlanBindingsV1,
    limits: &SchemaLimits,
) -> Result<(), WorkerError> {
    let matches = plan.plan_hash(limits)? == expected.plan_hash
        && plan.protocol_bundle_hash == expected.protocol_bundle_hash
        && plan.job_id == expected.job_id
        && plan.attempt == expected.attempt
        && plan.input_manifest_hash == expected.input_manifest_hash
        && plan.wwd == expected.wwd
        && plan.tribute_count == expected.tribute_count
        && plan.planner_spec_version == expected.planner_spec_version
        && plan.reducer_spec_version == expected.reducer_spec_version;
    if matches {
        Ok(())
    } else {
        Err(WorkerError::UnitBindingMismatch)
    }
}

pub(super) fn require_authenticated_input(
    spec: &UnitSpecV1,
    purpose: InputPurpose,
    source_id: B256,
    record_count: u32,
    encoded_bytes: u64,
) -> Result<(), WorkerError> {
    let matches = spec
        .canonical_ordered_inputs
        .iter()
        .filter(|input| {
            input.purpose == purpose
                && input.source_kind == InputSourceKind::AuthenticatedRoot
                && input.source_id == source_id
                && input.record_count_limit >= record_count
                && input.max_encoded_bytes >= encoded_bytes
        })
        .count();
    if matches == 1 {
        Ok(())
    } else {
        Err(WorkerError::UnitBindingMismatch)
    }
}
