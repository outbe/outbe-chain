//! One socket-activated process handles one immutable work-unit request.

use std::collections::BTreeMap;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use alloy_primitives::B256;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{decode_tribute_v1, CanonicalBodyError};
use outbe_fidelity::{evaluate_fidelity_opening_v1, FidelityOpeningEvaluationError};
use outbe_lysis::program_v1::artifacts::{
    decode_enumerated_run, encode_enumerated_run, encode_fidelity_map_output, enumerate_tributes,
    LysisArtifactErrorV1,
};
use outbe_lysis::program_v1::phases::fidelity_map;
use outbe_lysis::program_v1::planner::{LysisPlannerBindingsV1, LysisPlannerV1, PlannerErrorV1};
use outbe_lysis::program_v1::{ObservationValueV1, ObservedTributeV1, TributeInputV1};
use outbe_ocomp_protocol::common::BoundedBytes;
use outbe_ocomp_protocol::input::{
    AuthenticatedInputChunkV1, AuthenticatedOpeningV1, InputChunkKind, InputChunkRefV1,
    InputManifestV1, OpeningSourceKind,
};
use outbe_ocomp_protocol::unit::{
    InputPurpose, InputSourceKind, PlanCommitmentV1, UnitArtifactV1, UnitInterval, UnitPhase,
    UnitSpecV1, WorkOutputHeaderV1,
};
use outbe_ocomp_protocol::{
    verify_ordered_list_membership, ListKind, ObjectKind, RunUnitV1, SchemaLimits,
    UnitFinishedStatus, UnitFinishedV1, WorkerMessageKind,
};
use thiserror::Error;

use crate::bundle::PinnedProtocolBundle;
use crate::cas::{CasError, CasLimits, FilesystemCasReader};
use crate::control::{
    poc_schema_limits, require_effective_user, uid_for_user, ControlError, ControlServerSession,
    EndpointIdentity, ServerPolicy,
};
use crate::inbox::{WorkerInbox, WorkerInboxError, WorkerInboxLimits};
use crate::input_artifacts::{
    decode_fidelity_subject_key, decode_verified_input_chunk, derive_input_chunk_ref,
    InputArtifactError,
};

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub expected_effective_user: String,
    pub expected_supervisor_user: String,
    pub identity: EndpointIdentity,
    pub session_generation: u64,
    pub cas_root: PathBuf,
    pub cas_limits: CasLimits,
    pub inbox_root: PathBuf,
    pub inbox_limits: WorkerInboxLimits,
    pub connection_fd: RawFd,
    pub protocol_bundle: PinnedProtocolBundle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerOutcome {
    pub unit_id: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpectedPlanBindingsV1 {
    plan_hash: B256,
    protocol_bundle_hash: B256,
    job_id: B256,
    attempt: u32,
    input_manifest_hash: B256,
    wwd: u32,
    tribute_count: u32,
    planner_spec_version: u16,
    reducer_spec_version: u16,
}

struct UnitExecutionAuthority<'a> {
    plan: &'a PlanCommitmentV1,
    unit_index: u32,
    manifest: &'a InputManifestV1,
    input_chunks: &'a [(InputChunkRefV1, AuthenticatedInputChunkV1)],
    producer_artifacts: &'a [UnitArtifactV1],
    bundle: &'a outbe_ocomp_protocol::profile::ProtocolBundleV1,
    limits: &'a SchemaLimits,
}

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error("worker request is not valid OCOMP protocol: {0}")]
    Protocol(#[from] outbe_ocomp_protocol::ProtocolError),
    #[error("worker inherited descriptor {0} is not a valid connected socket")]
    InvalidInheritedDescriptor(RawFd),
    #[error("worker received message kind {actual:#06x}, expected RunUnitV1")]
    UnexpectedMessage { actual: u16 },
    #[error("worker request binding does not match its canonical UnitSpecV1")]
    UnitBindingMismatch,
    #[error("worker request carries the reserved zero plan hash")]
    ZeroPlanHash,
    #[error("worker pinned protocol bundle does not match endpoint identity")]
    BundleIdentityMismatch,
    #[error(transparent)]
    InputArtifact(#[from] InputArtifactError),
    #[error(transparent)]
    Inbox(#[from] WorkerInboxError),
    #[error(transparent)]
    LysisArtifact(#[from] LysisArtifactErrorV1),
    #[error(transparent)]
    Planner(#[from] PlannerErrorV1),
    #[error(transparent)]
    CanonicalBody(#[from] CanonicalBodyError),
    #[error(transparent)]
    FidelityOpening(#[from] FidelityOpeningEvaluationError),
    #[error("worker does not yet implement Lysis phase {0:?}")]
    UnsupportedPhase(UnitPhase),
}

pub fn run_one_from_inherited_fd(config: WorkerConfig) -> Result<WorkerOutcome, WorkerError> {
    require_effective_user(&config.expected_effective_user)?;
    if config.protocol_bundle.hash() != config.identity.protocol_bundle_hash {
        return Err(WorkerError::BundleIdentityMismatch);
    }
    let expected_supervisor_uid = uid_for_user(&config.expected_supervisor_user)?;
    let stream = duplicate_connected_stream(config.connection_fd)?;
    let reader = FilesystemCasReader::open(&config.cas_root, config.cas_limits)?;
    let inbox = WorkerInbox::open(&config.inbox_root, config.inbox_limits)?;
    let limits = poc_schema_limits();
    let mut session = ControlServerSession::accept(
        stream,
        ServerPolicy::worker(
            expected_supervisor_uid,
            config.identity,
            config.session_generation,
            limits,
        ),
    )?;
    session.handshake()?;
    let frame = session.receive_request()?;
    if frame.message_kind != WorkerMessageKind::RunUnit as u16 {
        return Err(WorkerError::UnexpectedMessage {
            actual: frame.message_kind,
        });
    }
    let request = RunUnitV1::decode_body(&frame.body, &limits)?;
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
        },
    )
    .and_then(|artifact| {
        artifact
            .encode_canonical(&limits)
            .map_err(WorkerError::from)
    })
    .and_then(|bytes| inbox.adopt(unit_id, &bytes).map_err(WorkerError::from))
    {
        Ok(staged) => {
            let reference = staged.reference();
            UnitFinishedV1 {
                unit_id,
                status: UnitFinishedStatus::Success,
                exact_staged_bytes: reference.encoded_bytes,
                transport_digest: reference.transport_digest,
            }
        }
        Err(_) => UnitFinishedV1 {
            unit_id,
            status: UnitFinishedStatus::Failed,
            exact_staged_bytes: 0,
            transport_digest: B256::ZERO,
        },
    };
    session.send_response(
        frame.request_id,
        WorkerMessageKind::UnitFinished as u16,
        finished.encode_body(&limits)?,
    )?;
    Ok(WorkerOutcome { unit_id })
}

fn execute_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    match spec.phase {
        UnitPhase::Enumerate => execute_enumerate_unit(
            spec,
            authority.manifest,
            authority.input_chunks,
            authority.producer_artifacts,
            authority.limits,
        ),
        UnitPhase::FidelityMap => execute_fidelity_map_unit(spec, authority),
        phase => Err(WorkerError::UnsupportedPhase(phase)),
    }
}

fn execute_enumerate_unit(
    spec: &UnitSpecV1,
    manifest: &InputManifestV1,
    input_chunks: &[(InputChunkRefV1, AuthenticatedInputChunkV1)],
    producer_artifacts: &[UnitArtifactV1],
    limits: &SchemaLimits,
) -> Result<UnitArtifactV1, WorkerError> {
    if !producer_artifacts.is_empty() || input_chunks.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let (reference, chunk) = &input_chunks[0];
    if chunk.kind != InputChunkKind::Tribute {
        return Err(WorkerError::UnitBindingMismatch);
    }
    require_authenticated_input(
        spec,
        InputPurpose::TributeStream,
        reference.semantic_digest,
        reference.record_count,
        reference.encoded_bytes,
    )?;
    let UnitInterval::EntityIdRange(range) = &spec.interval else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let mut tributes = Vec::new();
    tributes
        .try_reserve_exact(chunk.canonical_records_or_openings.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for record in &chunk.canonical_records_or_openings {
        let tribute = decode_tribute_v1(&record.0)?;
        let id = tribute.tribute_id.as_bytes();
        if id < &range.start.0 || range.end.is_some_and(|end| id >= &end.0) {
            return Err(WorkerError::UnitBindingMismatch);
        }
        tributes.push(TributeInputV1::from(&tribute));
    }
    if tributes
        .first()
        .map(|tribute| tribute.tribute_id.as_bytes())
        != Some(&range.start.0)
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let start_ordinal = chunk
        .ordinal
        .checked_mul(256)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let run = enumerate_tributes(start_ordinal, WorldwideDay::new(manifest.wwd), &tributes)?;
    let coverage_root = run.coverage_root()?;
    let output_count =
        u32::try_from(run.ordered_records.len()).map_err(|_| WorkerError::UnitBindingMismatch)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: output_count,
            output_coverage_count: output_count,
        },
        BoundedBytes(encode_enumerated_run(&run, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn execute_fidelity_map_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        plan,
        unit_index,
        manifest,
        input_chunks,
        producer_artifacts,
        bundle,
        limits,
    } = authority;
    let shard_ordinal = unit_index
        .checked_sub(plan.primary_work_unit_count)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if shard_ordinal >= plan.primary_work_unit_count || producer_artifacts.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let enumerate_unit_id = exact_unit_output_source(spec, InputPurpose::EnumeratedTributes)?;
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    if planner.fidelity_map_unit_at(shard_ordinal, enumerate_unit_id, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let primary_spec = planner.primary_unit_at(
        shard_ordinal,
        |ordinal| {
            input_chunks
                .iter()
                .find(|(reference, chunk)| {
                    reference.kind == InputChunkKind::Tribute
                        && chunk.kind == InputChunkKind::Tribute
                        && reference.ordinal == ordinal
                })
                .map(|(reference, _)| reference.clone())
        },
        limits,
    )?;
    let producer = &producer_artifacts[0];
    producer.validate_against(&primary_spec, limits)?;
    if producer.unit_id != enumerate_unit_id || producer.phase != UnitPhase::Enumerate {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let enumerated = decode_enumerated_run(producer.phase_payload(limits)?, limits)?;
    let producer_header = producer.output_header(limits)?;
    if enumerated.coverage_root()? != producer_header.output_coverage_root
        || enumerated.ordered_records.len()
            != usize::try_from(producer_header.output_coverage_count)
                .map_err(|_| WorkerError::UnitBindingMismatch)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let mut leagues = BTreeMap::new();
    let mut fidelity_opening_count = 0_u32;
    let mut fidelity_encoded_bytes = 0_u64;
    for (reference, chunk) in input_chunks {
        match chunk.kind {
            InputChunkKind::Tribute => {}
            InputChunkKind::Fidelity => {
                fidelity_encoded_bytes = fidelity_encoded_bytes
                    .checked_add(reference.encoded_bytes)
                    .ok_or(WorkerError::UnitBindingMismatch)?;
                for encoded in &chunk.canonical_records_or_openings {
                    fidelity_opening_count = fidelity_opening_count
                        .checked_add(1)
                        .ok_or(WorkerError::UnitBindingMismatch)?;
                    let opening =
                        AuthenticatedOpeningV1::decode_canonical_record(&encoded.0, limits)?;
                    if opening.source_kind != OpeningSourceKind::Fidelity {
                        return Err(WorkerError::UnitBindingMismatch);
                    }
                    opening.validate_against_bundle(bundle, limits)?;
                    let raw = opening.decode_and_validate_raw_opening(
                        manifest.checkpoint.finalized_state_root,
                        limits,
                    )?;
                    let owners = decode_fidelity_subject_key(&opening.canonical_subject_key.0)?;
                    let slot_values = raw
                        .ordered_slots
                        .iter()
                        .map(|slot| (slot.slot, slot.value))
                        .collect::<Vec<_>>();
                    for observation in evaluate_fidelity_opening_v1(
                        &owners,
                        &slot_values,
                        plan.logical_evaluation_time,
                    )? {
                        if leagues
                            .insert(observation.owner, observation.league)
                            .is_some()
                        {
                            return Err(WorkerError::UnitBindingMismatch);
                        }
                    }
                }
            }
            InputChunkKind::Oracle => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    require_authenticated_input(
        spec,
        InputPurpose::FidelityOpenings,
        manifest.fidelity_opening_root,
        fidelity_opening_count,
        fidelity_encoded_bytes,
    )?;

    let mut observed = Vec::new();
    observed
        .try_reserve_exact(enumerated.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for record in &enumerated.ordered_records {
        let league = leagues
            .get(&record.tribute.owner)
            .copied()
            .ok_or(WorkerError::UnitBindingMismatch)?;
        observed.push(ObservedTributeV1 {
            tribute: record.tribute.clone(),
            first_league: ObservationValueV1::Value(league),
            second_league: ObservationValueV1::Value(league),
            conditional_entry_price_minor: ObservationValueV1::Unavailable,
            nod_target_available: true,
        });
    }
    let output =
        fidelity_map(enumerated.start_ordinal, &observed).map_err(LysisArtifactErrorV1::from)?;
    let output_coverage_root = output.coverage_root()?;
    if output_coverage_root != producer_header.output_coverage_root {
        return Err(WorkerError::UnitBindingMismatch);
    }
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: producer_header.output_coverage_root,
            output_coverage_root,
            source_coverage_count: producer_header.output_coverage_count,
            output_coverage_count: output.aggregate.tribute_count,
        },
        BoundedBytes(encode_fidelity_map_output(&output, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn planner_from_authority(
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
        lysis_budget: plan.lysis_budget,
        logical_evaluation_time: plan.logical_evaluation_time,
        tribute_count: plan.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: plan.planner_spec_version,
        reducer_spec_version: plan.reducer_spec_version,
    })
    .map_err(WorkerError::from)
}

fn exact_unit_output_source(spec: &UnitSpecV1, purpose: InputPurpose) -> Result<B256, WorkerError> {
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

fn require_plan_binding(
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

fn require_authenticated_input(
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

#[allow(unsafe_code)]
fn duplicate_connected_stream(fd: RawFd) -> Result<UnixStream, WorkerError> {
    // SAFETY: `fcntl(F_DUPFD_CLOEXEC)` does not take ownership of `fd`; on
    // success it returns a fresh descriptor which is immediately wrapped.
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(WorkerError::InvalidInheritedDescriptor(fd));
    }
    // SAFETY: `duplicated` is a fresh descriptor owned by this function.
    let owned = unsafe { OwnedFd::from_raw_fd(duplicated) };
    Ok(UnixStream::from(owned))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, U256};
    use outbe_ocomp_protocol::unit::PlanCommitmentV1;

    use super::{poc_schema_limits, require_plan_binding, ExpectedPlanBindingsV1};

    fn plan() -> PlanCommitmentV1 {
        PlanCommitmentV1 {
            protocol_bundle_hash: B256::repeat_byte(1),
            job_id: B256::repeat_byte(2),
            attempt: 3,
            input_manifest_hash: B256::repeat_byte(4),
            wwd: 20_260_724,
            lysis_budget: U256::from(99_000_000_u64),
            logical_evaluation_time: 1_784_765_900,
            tribute_count: 257,
            max_tributes_per_work_shard: 256,
            primary_work_unit_count: 2,
            primary_work_unit_root: B256::repeat_byte(5),
            planner_spec_version: 1,
            reducer_spec_version: 1,
        }
    }

    #[test]
    fn changed_frozen_plan_context_is_rejected_even_when_job_and_manifest_bindings_match() {
        let limits = poc_schema_limits();
        let committed = plan();
        let expected = ExpectedPlanBindingsV1 {
            plan_hash: committed.plan_hash(&limits).unwrap(),
            protocol_bundle_hash: committed.protocol_bundle_hash,
            job_id: committed.job_id,
            attempt: committed.attempt,
            input_manifest_hash: committed.input_manifest_hash,
            wwd: committed.wwd,
            tribute_count: committed.tribute_count,
            planner_spec_version: committed.planner_spec_version,
            reducer_spec_version: committed.reducer_spec_version,
        };
        require_plan_binding(&committed, expected, &limits).unwrap();

        let mut changed = committed;
        changed.lysis_budget += U256::from(1);
        assert!(require_plan_binding(&changed, expected, &limits).is_err());
    }
}
