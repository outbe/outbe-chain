//! One socket-activated process handles one immutable work-unit request.

use std::collections::BTreeMap;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{decode_tribute_v1, CanonicalBodyError};
use outbe_fidelity::{evaluate_fidelity_opening_v1, FidelityOpeningEvaluationError};
use outbe_lysis::program_v1::artifacts::{
    decode_amount_run, decode_enumerated_run, decode_fidelity_map_output,
    decode_finalized_output_run, decode_fixed_reduce_output, decode_gratis_prefix_down_output,
    decode_gratis_segment_summary, encode_amount_run, encode_enumerated_run,
    encode_fidelity_map_output, encode_finalized_output_run, encode_fixed_reduce_output,
    encode_gratis_prefix_down_output, encode_gratis_segment_summary, enumerate_tributes,
    gratis_summary_coverage, FixedReduceOutputV1, GratisPrefixDownOutputV1, LysisArtifactErrorV1,
    RawCoverageCarrierV1,
};
use outbe_lysis::program_v1::phases::{
    amount_map, fidelity_map, fidelity_reduce_pair, finalize_fi_fraction_table, gratis_prefix_down,
    gratis_summary, gratis_summary_reduce_pair, output_finalize, shuffle_buckets, shuffle_owners,
    FidelityReduceValueV1, GratisLeafPrefixV1, GratisSummaryValueV1,
};
use outbe_lysis::program_v1::planner::{
    LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1, PlannedProducerV1,
    PlannedUnitPositionV1, PlannerErrorV1, PRIMARY_WORK_SHARD_SIZE,
};
use outbe_lysis::program_v1::result::{
    decode_root_reduce_output, encode_root_reduce_output, LysisListSubtreeCarrierV1,
    RootReduceOutputV1, RootReduceSummaryV1,
};
use outbe_lysis::program_v1::{ObservationValueV1, ObservedTributeV1, TributeInputV1};
use outbe_ocomp_protocol::common::{BoundedBytes, EntityId36 as ProtocolEntityId36};
use outbe_ocomp_protocol::input::{
    AuthenticatedInputChunkV1, AuthenticatedOpeningV1, InputChunkKind, InputChunkRefV1,
    InputManifestV1, OpeningSourceKind,
};
use outbe_ocomp_protocol::unit::{
    BinaryReducerNode, CanonicalInputRefV1, FidelityIndexHalfOpenRange, InputPurpose,
    InputSourceKind, PlanCommitmentV1, UnitArtifactV1, UnitInterval, UnitPhase, UnitSpecV1,
    WorkOutputHeaderV1,
};
use outbe_ocomp_protocol::{
    result::ContributorActionV1 as ProtocolContributorActionV1,
    shuffle::{
        build_bucket_shuffle_run, build_owner_shuffle_run, merge_verified_shuffle_runs,
        verified_shuffle_run_records, ShuffleBucketRecordV1, ShuffleRunArtifactV1,
        ShuffleRunBuildContextV1, ShuffleRunKindV1, ShuffleSourceCoverageV1,
        VerifiedShuffleRecordV1,
    },
};
use outbe_ocomp_protocol::{
    verify_ordered_list_membership, ListKind, ObjectKind, RunUnitV1, SchemaLimits,
    UnitFinishedStatus, UnitFinishedV1, WorkerMessageKind,
};
use outbe_oracle::{evaluate_oracle_opening_v1, OracleOpeningEvaluationError};
use thiserror::Error;

use crate::bundle::PinnedProtocolBundle;
use crate::cas::{CasError, CasLimits, FilesystemCasReader};
use crate::control::{
    poc_schema_limits, require_effective_user, uid_for_user, ControlError, ControlServerSession,
    EndpointIdentity, ServerPolicy,
};
use crate::inbox::{WorkerInbox, WorkerInboxError, WorkerInboxLimits};
use crate::input_artifacts::{
    decode_fidelity_subject_key, decode_oracle_subject_key, decode_verified_input_chunk,
    derive_input_chunk_ref, InputArtifactError,
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
    reader: &'a FilesystemCasReader,
    inbox: &'a WorkerInbox,
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
    #[error(transparent)]
    OracleOpening(#[from] OracleOpeningEvaluationError),
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
            reader: &reader,
            inbox: &inbox,
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
        UnitPhase::FixedReduce => execute_fixed_reduce_unit(spec, authority),
        UnitPhase::AmountMap => execute_amount_map_unit(spec, authority),
        UnitPhase::GratisPrefix => execute_gratis_prefix_unit(spec, authority),
        UnitPhase::GratisPrefixDown => execute_gratis_prefix_down_unit(spec, authority),
        UnitPhase::OutputFinalize => execute_output_finalize_unit(spec, authority),
        UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle => execute_shuffle_unit(spec, authority),
        UnitPhase::RootReduce => execute_root_reduce_unit(spec, authority),
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
        ..
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

struct FixedReduceInputV1 {
    value: FidelityReduceValueV1,
    coverage: RawCoverageCarrierV1,
}

fn execute_fixed_reduce_unit(
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
        ..
    } = authority;
    if !input_chunks.is_empty() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let fixed_reduce_offset = plan
        .primary_work_unit_count
        .checked_mul(2)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let phase_ordinal = unit_index
        .checked_sub(fixed_reduce_offset)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::FixedReduce, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::FixedReduce,
        level,
        index,
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let reducer_inputs = spec
        .canonical_ordered_inputs
        .iter()
        .filter(|input| input.purpose == InputPurpose::FidelityPartials)
        .collect::<Vec<_>>();
    if reducer_inputs.len() != 2 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let producer_ids = reducer_inputs
        .iter()
        .map(|input| match input.source_kind {
            InputSourceKind::UnitOutput if !input.source_id.is_zero() => Ok(Some(input.source_id)),
            InputSourceKind::CanonicalEmpty => Ok(None),
            _ => Err(WorkerError::UnitBindingMismatch),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let producer_ids: [Option<B256>; 2] = producer_ids
        .try_into()
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    if planner.fixed_reduce_unit_at(phase_ordinal, producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let expected_producers = topology.required_producers(position)?;
    let mut artifacts = producer_artifacts.iter();
    let mut decoded_inputs = Vec::with_capacity(2);
    for (expected, input) in expected_producers.into_iter().zip(reducer_inputs) {
        match expected {
            PlannedProducerV1::CanonicalEmpty {
                purpose: InputPurpose::FidelityPartials,
                padded_ordinal,
            } => {
                if input.source_kind != InputSourceKind::CanonicalEmpty {
                    return Err(WorkerError::UnitBindingMismatch);
                }
                decoded_inputs.push(FixedReduceInputV1 {
                    value: FidelityReduceValueV1::Empty,
                    coverage: RawCoverageCarrierV1::canonical_empty(
                        plan.tribute_count,
                        padded_ordinal,
                    )?,
                });
            }
            PlannedProducerV1::Unit(producer_position) => {
                if input.source_kind != InputSourceKind::UnitOutput {
                    return Err(WorkerError::UnitBindingMismatch);
                }
                let artifact = artifacts.next().ok_or(WorkerError::UnitBindingMismatch)?;
                decoded_inputs.push(decode_fixed_reduce_producer(
                    artifact,
                    input.source_id,
                    producer_position,
                    spec,
                    plan,
                    limits,
                )?);
            }
            _ => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    if artifacts.next().is_some() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let [left, right]: [FixedReduceInputV1; 2] = decoded_inputs
        .try_into()
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    let single_primary_root = plan.primary_work_unit_count == 1 && level == 1 && index == 0;
    let (value, coverage) = if single_primary_root {
        if !matches!(&right.value, FidelityReduceValueV1::Empty)
            || !matches!(&left.value, FidelityReduceValueV1::Aggregate(_))
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        (left.value, left.coverage)
    } else {
        (
            fidelity_reduce_pair(left.value, right.value).map_err(LysisArtifactErrorV1::from)?,
            RawCoverageCarrierV1::merge(&left.coverage, &right.coverage)?,
        )
    };
    let is_root = level == topology.tree().height() && index == 0;
    let aggregate = match value {
        FidelityReduceValueV1::Empty => None,
        FidelityReduceValueV1::Aggregate(aggregate) => Some(aggregate),
    };
    let ordered_fractions = if is_root {
        finalize_fi_fraction_table(
            aggregate.as_ref().ok_or(WorkerError::UnitBindingMismatch)?,
            plan.lysis_budget,
        )
        .map_err(LysisArtifactErrorV1::from)?
    } else {
        Vec::new()
    };
    let output_count = aggregate
        .as_ref()
        .map_or(0, |aggregate| aggregate.tribute_count);
    let coverage_root = if is_root {
        coverage.final_root(plan.tribute_count)?
    } else {
        coverage.tree_root
    };
    let output = FixedReduceOutputV1 {
        aggregate,
        coverage,
        ordered_fractions,
    };
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: output_count,
            output_coverage_count: output_count,
        },
        BoundedBytes(encode_fixed_reduce_output(&output, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn decode_fixed_reduce_producer(
    artifact: &UnitArtifactV1,
    expected_unit_id: B256,
    position: PlannedUnitPositionV1,
    consumer_spec: &UnitSpecV1,
    plan: &PlanCommitmentV1,
    limits: &SchemaLimits,
) -> Result<FixedReduceInputV1, WorkerError> {
    artifact.validate_semantics(limits)?;
    if artifact.unit_id != expected_unit_id
        || artifact.protocol_bundle_hash != consumer_spec.protocol_bundle_hash
        || artifact.job_id != consumer_spec.job_id
        || artifact.attempt != consumer_spec.attempt
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let (phase, interval) = match position {
        PlannedUnitPositionV1::Primary {
            phase: UnitPhase::FidelityMap,
            ordinal,
        } => {
            let start = ordinal
                .checked_mul(PRIMARY_WORK_SHARD_SIZE)
                .ok_or(WorkerError::UnitBindingMismatch)?;
            let end = start
                .saturating_add(PRIMARY_WORK_SHARD_SIZE)
                .min(plan.tribute_count);
            (
                UnitPhase::FidelityMap,
                UnitInterval::FidelityIndexRange(FidelityIndexHalfOpenRange { start, end }),
            )
        }
        PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::FixedReduce,
            level,
            index,
        } => (
            UnitPhase::FixedReduce,
            UnitInterval::BinaryReducerNode(BinaryReducerNode { level, index }),
        ),
        _ => return Err(WorkerError::UnitBindingMismatch),
    };
    let mut interval_binding = consumer_spec.clone();
    interval_binding.phase = phase;
    interval_binding.interval = interval;
    if artifact.phase != phase
        || artifact.interval_commitment != interval_binding.interval_commitment(limits)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let header = artifact.output_header(limits)?;
    match phase {
        UnitPhase::FidelityMap => {
            let output = decode_fidelity_map_output(artifact.phase_payload(limits)?, limits)?;
            if output.coverage_root()? != header.output_coverage_root
                || output.aggregate.tribute_count != header.output_coverage_count
            {
                return Err(WorkerError::UnitBindingMismatch);
            }
            let records = output
                .observations
                .iter()
                .map(|observation| (observation.raw_ordinal, *observation.tribute_id.as_bytes()))
                .collect::<Vec<_>>();
            let coverage = RawCoverageCarrierV1::from_records(plan.tribute_count, &records)?;
            Ok(FixedReduceInputV1 {
                value: FidelityReduceValueV1::Aggregate(output.aggregate),
                coverage,
            })
        }
        UnitPhase::FixedReduce => {
            let output = decode_fixed_reduce_output(artifact.phase_payload(limits)?, limits)?;
            let output_count = output
                .aggregate
                .as_ref()
                .map_or(0, |aggregate| aggregate.tribute_count);
            if !output.ordered_fractions.is_empty()
                || output.coverage.tree_root != header.output_coverage_root
                || output_count != header.output_coverage_count
            {
                return Err(WorkerError::UnitBindingMismatch);
            }
            Ok(FixedReduceInputV1 {
                value: output.aggregate.map_or(
                    FidelityReduceValueV1::Empty,
                    FidelityReduceValueV1::Aggregate,
                ),
                coverage: output.coverage,
            })
        }
        _ => Err(WorkerError::UnitBindingMismatch),
    }
}

fn execute_amount_map_unit(
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
        ..
    } = authority;
    if producer_artifacts.len() != 3 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let amount_offset = plan
        .primary_work_unit_count
        .checked_mul(2)
        .and_then(|offset| offset.checked_add(topology.phase_unit_count(UnitPhase::FixedReduce)))
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let shard_ordinal = unit_index
        .checked_sub(amount_offset)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if shard_ordinal >= plan.primary_work_unit_count {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let enumerate_unit_id = exact_unit_output_source(spec, InputPurpose::EnumeratedTributes)?;
    let fidelity_unit_id = exact_unit_output_source(spec, InputPurpose::FidelityPartials)?;
    let fraction_root_unit_id = exact_unit_output_source(spec, InputPurpose::FiFractionTable)?;

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
    if primary_spec.unit_id(limits)? != enumerate_unit_id
        || planner.amount_map_unit_at(
            shard_ordinal,
            &primary_spec,
            fidelity_unit_id,
            fraction_root_unit_id,
            limits,
        )? != spec.clone()
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let enumerate_artifact = &producer_artifacts[0];
    enumerate_artifact.validate_against(&primary_spec, limits)?;
    let enumerated = decode_enumerated_run(enumerate_artifact.phase_payload(limits)?, limits)?;
    let enumerate_header = enumerate_artifact.output_header(limits)?;
    if enumerated.coverage_root()? != enumerate_header.output_coverage_root
        || enumerate_header.output_coverage_count
            != u32::try_from(enumerated.ordered_records.len())
                .map_err(|_| WorkerError::UnitBindingMismatch)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let fidelity_spec = planner.fidelity_map_unit_at(shard_ordinal, enumerate_unit_id, limits)?;
    let fidelity_artifact = &producer_artifacts[1];
    fidelity_artifact.validate_against(&fidelity_spec, limits)?;
    if fidelity_artifact.unit_id != fidelity_unit_id {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let fidelity = decode_fidelity_map_output(fidelity_artifact.phase_payload(limits)?, limits)?;
    let fidelity_header = fidelity_artifact.output_header(limits)?;
    if fidelity.coverage_root()? != fidelity_header.output_coverage_root
        || fidelity_header.output_coverage_root != enumerate_header.output_coverage_root
        || fidelity.aggregate.tribute_count != fidelity_header.output_coverage_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let root_artifact = &producer_artifacts[2];
    root_artifact.validate_semantics(limits)?;
    let root_interval = UnitInterval::BinaryReducerNode(BinaryReducerNode {
        level: topology.tree().height(),
        index: 0,
    });
    let mut root_interval_binding = spec.clone();
    root_interval_binding.phase = UnitPhase::FixedReduce;
    root_interval_binding.interval = root_interval;
    if root_artifact.unit_id != fraction_root_unit_id
        || root_artifact.protocol_bundle_hash != spec.protocol_bundle_hash
        || root_artifact.job_id != spec.job_id
        || root_artifact.attempt != spec.attempt
        || root_artifact.phase != UnitPhase::FixedReduce
        || root_artifact.interval_commitment != root_interval_binding.interval_commitment(limits)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let root_output = decode_fixed_reduce_output(root_artifact.phase_payload(limits)?, limits)?;
    let root_header = root_artifact.output_header(limits)?;
    let root_aggregate = root_output
        .aggregate
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if root_output.ordered_fractions.is_empty()
        || root_aggregate.tribute_count != plan.tribute_count
        || root_output.coverage.final_root(plan.tribute_count)? != root_header.output_coverage_root
        || root_header.output_coverage_count != plan.tribute_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let oracle_chunks = input_chunks
        .iter()
        .filter(|(_, chunk)| chunk.kind == InputChunkKind::Oracle)
        .collect::<Vec<_>>();
    if oracle_chunks.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let (oracle_reference, oracle_chunk) = oracle_chunks[0];
    if oracle_chunk.canonical_records_or_openings.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    require_authenticated_input(
        spec,
        InputPurpose::OracleOpenings,
        manifest.oracle_opening_root,
        1,
        oracle_reference.encoded_bytes,
    )?;
    let opening = AuthenticatedOpeningV1::decode_canonical_record(
        &oracle_chunk.canonical_records_or_openings[0].0,
        limits,
    )?;
    if opening.source_kind != OpeningSourceKind::Oracle {
        return Err(WorkerError::UnitBindingMismatch);
    }
    opening.validate_against_bundle(bundle, limits)?;
    let (oracle_wwd, settlement_isos) =
        decode_oracle_subject_key(&opening.canonical_subject_key.0)?;
    if oracle_wwd != manifest.wwd {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let raw = opening
        .decode_and_validate_raw_opening(manifest.checkpoint.finalized_state_root, limits)?;
    let raw_slots = raw
        .ordered_slots
        .iter()
        .map(|slot| (slot.slot, slot.value))
        .collect::<Vec<_>>();
    let oracle = evaluate_oracle_opening_v1(
        WorldwideDay::new(manifest.wwd),
        &settlement_isos,
        &raw_slots,
    )?;
    let mandatory_entry_price = oracle
        .entry_price(840)
        .ok_or(WorkerError::UnitBindingMismatch)?;

    let mut observed = Vec::new();
    observed
        .try_reserve_exact(enumerated.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for (record, fidelity) in enumerated
        .ordered_records
        .iter()
        .zip(&fidelity.observations)
    {
        if record.raw_ordinal != fidelity.raw_ordinal
            || record.tribute.tribute_id != fidelity.tribute_id
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        observed.push(ObservedTributeV1 {
            tribute: record.tribute.clone(),
            first_league: ObservationValueV1::Value(fidelity.pre_distribution_league),
            second_league: ObservationValueV1::Value(fidelity.issuance_league),
            conditional_entry_price_minor: oracle
                .entry_price(record.tribute.reference_currency)
                .map_or(ObservationValueV1::Unavailable, ObservationValueV1::Value),
            nod_target_available: true,
        });
    }
    let amount = amount_map(
        enumerated.start_ordinal,
        &observed,
        &fidelity.observations,
        &root_output.ordered_fractions,
        mandatory_entry_price,
    )
    .map_err(LysisArtifactErrorV1::from)?;
    let output_coverage_root = amount.coverage_root()?;
    if output_coverage_root != enumerate_header.output_coverage_root {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let output_count = u32::try_from(amount.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: enumerate_header.output_coverage_root,
            output_coverage_root,
            source_coverage_count: enumerate_header.output_coverage_count,
            output_coverage_count: output_count,
        },
        BoundedBytes(encode_amount_run(&amount, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn execute_gratis_prefix_unit(
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
        ..
    } = authority;
    if !input_chunks.is_empty() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let phase_ordinal = unit_index
        .checked_sub(gratis_prefix_offset(plan, topology)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::GratisPrefix, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::GratisPrefix,
        level,
        index,
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let purpose = if level == 0 {
        InputPurpose::AmountRecords
    } else {
        InputPurpose::GratisPrefixTable
    };
    let producer_inputs = scan_producer_inputs(spec, purpose)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| unit_or_empty_id(input))
        .collect::<Result<Vec<_>, _>>()?;
    if planner.gratis_prefix_unit_at(phase_ordinal, &producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        producer_artifacts,
        limits,
    )?;

    let (summary, coverage_root, coverage_count) = if level == 0 {
        let amount_artifact = resolved
            .first()
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        if amount_artifact.phase != UnitPhase::AmountMap {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let amount = decode_amount_run(amount_artifact.phase_payload(limits)?, limits)?;
        let amount_header = amount_artifact.output_header(limits)?;
        let expected_start = index
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        if amount.start_ordinal != expected_start
            || amount.end_ordinal > plan.tribute_count
            || amount.coverage_root()? != amount_header.output_coverage_root
            || amount_header.output_coverage_count
                != u32::try_from(amount.ordered_records.len())
                    .map_err(|_| WorkerError::UnitBindingMismatch)?
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let summary = gratis_summary(
            amount.start_ordinal,
            &amount
                .ordered_records
                .iter()
                .map(|record| record.gratis_load_minor)
                .collect::<Vec<_>>(),
        )
        .map_err(LysisArtifactErrorV1::from)?;
        (
            summary,
            amount_header.output_coverage_root,
            amount_header.output_coverage_count,
        )
    } else {
        let mut values = [GratisSummaryValueV1::Empty, GratisSummaryValueV1::Empty];
        let mut child_coverage = [None, None];
        for (child_index, artifact) in resolved.into_iter().enumerate() {
            let Some(artifact) = artifact else {
                continue;
            };
            let summary = decode_gratis_segment_summary(artifact.phase_payload(limits)?, limits)?;
            let header = artifact.output_header(limits)?;
            if summary.end_ordinal - summary.start_ordinal != header.output_coverage_count {
                return Err(WorkerError::UnitBindingMismatch);
            }
            values[child_index] = GratisSummaryValueV1::Summary(summary);
            child_coverage[child_index] =
                Some((header.output_coverage_root, header.output_coverage_count));
        }
        let summary = match gratis_summary_reduce_pair(values[0].clone(), values[1].clone())
            .map_err(LysisArtifactErrorV1::from)?
        {
            GratisSummaryValueV1::Summary(summary) => summary,
            GratisSummaryValueV1::Empty => return Err(WorkerError::UnitBindingMismatch),
        };
        let coverage = gratis_summary_coverage(spec.interval_commitment(limits)?, child_coverage)?;
        if coverage.count != summary.end_ordinal - summary.start_ordinal {
            return Err(WorkerError::UnitBindingMismatch);
        }
        (summary, coverage.root, coverage.count)
    };

    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: coverage_count,
            output_coverage_count: coverage_count,
        },
        BoundedBytes(encode_gratis_segment_summary(&summary, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn execute_gratis_prefix_down_unit(
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
        ..
    } = authority;
    if !input_chunks.is_empty() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let phase_ordinal = unit_index
        .checked_sub(gratis_prefix_down_offset(plan, topology)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::GratisPrefixDown, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::GratisPrefixDown,
        level,
        index,
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let producer_inputs = scan_producer_inputs(spec, InputPurpose::GratisPrefixTable)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| unit_or_empty_id(input))
        .collect::<Result<Vec<_>, _>>()?;
    if planner.gratis_prefix_down_unit_at(phase_ordinal, &producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        producer_artifacts,
        limits,
    )?;

    if level == 0 {
        let parent = resolved
            .first()
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let summary_artifact = resolved
            .get(1)
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let GratisPrefixDownOutputV1::Branch(children) =
            decode_gratis_prefix_down_output(parent.phase_payload(limits)?, limits)?
        else {
            return Err(WorkerError::UnitBindingMismatch);
        };
        let incoming = children
            [usize::try_from(index & 1).map_err(|_| WorkerError::UnitBindingMismatch)?]
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
        let summary =
            decode_gratis_segment_summary(summary_artifact.phase_payload(limits)?, limits)?;
        let summary_header = summary_artifact.output_header(limits)?;
        if incoming.start_ordinal != summary.start_ordinal
            || incoming.end_ordinal != summary.end_ordinal
            || summary.end_ordinal - summary.start_ordinal != summary_header.output_coverage_count
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let incoming_remaining = incoming
            .incoming_remaining
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let outgoing_remaining = incoming_remaining
            .checked_sub(summary.checked_segment_gratis_total)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let output = GratisPrefixDownOutputV1::Leaf(GratisLeafPrefixV1 {
            segment_ordinal: index,
            incoming_remaining,
            outgoing_remaining,
            first_error_ordinal: None,
        });
        return UnitArtifactV1::from_canonical_output(
            spec,
            WorkOutputHeaderV1 {
                source_coverage_root: summary_header.output_coverage_root,
                output_coverage_root: summary_header.output_coverage_root,
                source_coverage_count: summary_header.output_coverage_count,
                output_coverage_count: summary_header.output_coverage_count,
            },
            BoundedBytes(encode_gratis_prefix_down_output(&output, limits)?),
            limits,
        )
        .map_err(WorkerError::from);
    }

    let is_root = level == topology.tree().height() && index == 0;
    let (incoming_remaining, child_start) = if is_root {
        (Some(plan.lysis_budget), 0)
    } else {
        let parent = resolved
            .first()
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let GratisPrefixDownOutputV1::Branch(children) =
            decode_gratis_prefix_down_output(parent.phase_payload(limits)?, limits)?
        else {
            return Err(WorkerError::UnitBindingMismatch);
        };
        let incoming = children
            [usize::try_from(index & 1).map_err(|_| WorkerError::UnitBindingMismatch)?]
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
        (incoming.incoming_remaining, 1)
    };
    let mut values = [GratisSummaryValueV1::Empty, GratisSummaryValueV1::Empty];
    let mut child_coverage = [None, None];
    for child_index in 0..2 {
        let Some(artifact) = resolved
            .get(child_start + child_index)
            .and_then(|artifact| *artifact)
        else {
            continue;
        };
        let summary = decode_gratis_segment_summary(artifact.phase_payload(limits)?, limits)?;
        let header = artifact.output_header(limits)?;
        if summary.end_ordinal - summary.start_ordinal != header.output_coverage_count {
            return Err(WorkerError::UnitBindingMismatch);
        }
        values[child_index] = GratisSummaryValueV1::Summary(summary);
        child_coverage[child_index] =
            Some((header.output_coverage_root, header.output_coverage_count));
    }
    let combined = match gratis_summary_reduce_pair(values[0].clone(), values[1].clone())
        .map_err(LysisArtifactErrorV1::from)?
    {
        GratisSummaryValueV1::Summary(summary) => summary,
        GratisSummaryValueV1::Empty => return Err(WorkerError::UnitBindingMismatch),
    };
    let children = gratis_prefix_down(incoming_remaining, values[0].clone(), values[1].clone())
        .map_err(LysisArtifactErrorV1::from)?;
    if !is_root {
        let parent = resolved[0].ok_or(WorkerError::UnitBindingMismatch)?;
        let GratisPrefixDownOutputV1::Branch(parent_children) =
            decode_gratis_prefix_down_output(parent.phase_payload(limits)?, limits)?
        else {
            return Err(WorkerError::UnitBindingMismatch);
        };
        let assigned = parent_children
            [usize::try_from(index & 1).map_err(|_| WorkerError::UnitBindingMismatch)?]
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
        if assigned.start_ordinal != combined.start_ordinal
            || assigned.end_ordinal != combined.end_ordinal
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
    }
    let mut prefix_spec = spec.clone();
    prefix_spec.phase = UnitPhase::GratisPrefix;
    let coverage =
        gratis_summary_coverage(prefix_spec.interval_commitment(limits)?, child_coverage)?;
    if coverage.count != combined.end_ordinal - combined.start_ordinal {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let output = GratisPrefixDownOutputV1::Branch(children);
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage.root,
            output_coverage_root: coverage.root,
            source_coverage_count: coverage.count,
            output_coverage_count: coverage.count,
        },
        BoundedBytes(encode_gratis_prefix_down_output(&output, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn execute_output_finalize_unit(
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
        ..
    } = authority;
    if !input_chunks.is_empty() || producer_artifacts.len() != 2 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let shard_ordinal = unit_index
        .checked_sub(output_finalize_offset(plan, topology)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if shard_ordinal >= plan.primary_work_unit_count {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let amount_unit_id = exact_unit_output_source(spec, InputPurpose::AmountRecords)?;
    let prefix_unit_id = exact_unit_output_source(spec, InputPurpose::GratisPrefixTable)?;
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    planner.validate_output_finalize_unit(
        shard_ordinal,
        spec,
        amount_unit_id,
        prefix_unit_id,
        limits,
    )?;

    let amount_artifact = &producer_artifacts[0];
    amount_artifact.validate_semantics(limits)?;
    let mut amount_interval_binding = spec.clone();
    amount_interval_binding.phase = UnitPhase::AmountMap;
    if amount_artifact.unit_id != amount_unit_id
        || amount_artifact.protocol_bundle_hash != spec.protocol_bundle_hash
        || amount_artifact.job_id != spec.job_id
        || amount_artifact.attempt != spec.attempt
        || amount_artifact.phase != UnitPhase::AmountMap
        || amount_artifact.interval_commitment
            != amount_interval_binding.interval_commitment(limits)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let prefix_artifact = &producer_artifacts[1];
    validate_scan_artifact(
        spec,
        PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::GratisPrefixDown,
            level: 0,
            index: shard_ordinal,
        },
        prefix_unit_id,
        prefix_artifact,
        limits,
    )?;

    let amount = decode_amount_run(amount_artifact.phase_payload(limits)?, limits)?;
    let amount_header = amount_artifact.output_header(limits)?;
    let GratisPrefixDownOutputV1::Leaf(prefix) =
        decode_gratis_prefix_down_output(prefix_artifact.phase_payload(limits)?, limits)?
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let prefix_header = prefix_artifact.output_header(limits)?;
    if amount.start_ordinal / PRIMARY_WORK_SHARD_SIZE != shard_ordinal
        || amount.coverage_root()? != amount_header.output_coverage_root
        || amount_header.output_coverage_root != prefix_header.output_coverage_root
        || amount_header.output_coverage_count != prefix_header.output_coverage_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let output = output_finalize(&amount, &prefix, plan.logical_evaluation_time)
        .map_err(LysisArtifactErrorV1::from)?;
    let encoded = encode_finalized_output_run(&output, limits)?;
    if decode_finalized_output_run(&encoded, limits)? != output {
        return Err(WorkerError::UnitBindingMismatch);
    }
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: amount_header.output_coverage_root,
            output_coverage_root: amount_header.output_coverage_root,
            source_coverage_count: amount_header.output_coverage_count,
            output_coverage_count: amount_header.output_coverage_count,
        },
        BoundedBytes(encoded),
        limits,
    )
    .map_err(WorkerError::from)
}

fn execute_shuffle_unit(
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
        reader,
        inbox,
    } = authority;
    if !input_chunks.is_empty()
        || !matches!(
            spec.phase,
            UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle
        )
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let phase_offset = if spec.phase == UnitPhase::OwnerShuffle {
        owner_shuffle_offset(plan, topology)?
    } else {
        bucket_shuffle_offset(plan, topology)?
    };
    let phase_ordinal = unit_index
        .checked_sub(phase_offset)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(spec.phase, phase_ordinal)?;
    let PlannedUnitPositionV1::RunSpan {
        level,
        start_run,
        end_run,
        ..
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let run_span = outbe_ocomp_protocol::unit::CanonicalRunSpan { start_run, end_run };
    let purpose = if level == 0 {
        InputPurpose::FinalizedOutputRecords
    } else if spec.phase == UnitPhase::OwnerShuffle {
        InputPurpose::OwnerOrderedRecords
    } else {
        InputPurpose::BucketOrderedRecords
    };
    let producer_inputs = scan_producer_inputs(spec, purpose)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| {
            if input.source_kind != InputSourceKind::UnitOutput || input.source_id.is_zero() {
                Err(WorkerError::UnitBindingMismatch)
            } else {
                Ok(input.source_id)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    if planner.shuffle_unit_at(spec.phase, phase_ordinal, &producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        producer_artifacts,
        limits,
    )?;
    if resolved.iter().any(Option::is_none) {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let (coverage, owner_records, bucket_records) = if level == 0 {
        if resolved.len() != 1 || end_run != start_run + 1 {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let producer = resolved[0].ok_or(WorkerError::UnitBindingMismatch)?;
        let finalized = decode_finalized_output_run(producer.phase_payload(limits)?, limits)?;
        let header = producer.output_header(limits)?;
        let expected_start = start_run
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let expected_end = end_run
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(WorkerError::UnitBindingMismatch)?
            .min(plan.tribute_count);
        if finalized.start_ordinal != expected_start
            || finalized.end_ordinal != expected_end
            || finalized.coverage_root()? != header.output_coverage_root
            || finalized.end_ordinal - finalized.start_ordinal != header.output_coverage_count
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let coverage = ShuffleSourceCoverageV1::leaf(
            run_span.clone(),
            header.output_coverage_root,
            header.output_coverage_count,
            limits,
        )?;
        if spec.phase == UnitPhase::OwnerShuffle {
            let owner_run = shuffle_owners(&finalized).map_err(LysisArtifactErrorV1::from)?;
            let records = owner_run
                .ordered_contributors
                .into_iter()
                .map(|record| {
                    Ok(ProtocolContributorActionV1 {
                        owner: record.owner,
                        source_tribute_id: protocol_entity_id(record.source_tribute_id),
                        nominal_amount_minor: record.nominal_amount_minor,
                    })
                })
                .collect::<Vec<_>>();
            (coverage, Some(records), None)
        } else {
            let bucket_run = shuffle_buckets(&finalized).map_err(LysisArtifactErrorV1::from)?;
            let records = bucket_run
                .ordered_records
                .into_iter()
                .map(|record| {
                    Ok(ShuffleBucketRecordV1 {
                        bucket_key: record.bucket_key,
                        raw_ordinal: record.raw_ordinal,
                        tribute_id: protocol_entity_id(record.tribute_id),
                        nod_id: protocol_entity_id(record.nod_id),
                    })
                })
                .collect::<Vec<_>>();
            (coverage, None, Some(records))
        }
    } else {
        if resolved.len() != 2 {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let expected_kind = if spec.phase == UnitPhase::OwnerShuffle {
            ShuffleRunKindV1::Owner
        } else {
            ShuffleRunKindV1::Bucket
        };
        let left = decode_shuffle_producer_root(
            spec,
            resolved[0].ok_or(WorkerError::UnitBindingMismatch)?,
            expected_kind,
            expected[0],
            limits,
        )?;
        let right = decode_shuffle_producer_root(
            spec,
            resolved[1].ok_or(WorkerError::UnitBindingMismatch)?,
            expected_kind,
            expected[1],
            limits,
        )?;
        let coverage = ShuffleSourceCoverageV1::merge(
            &ShuffleSourceCoverageV1 {
                run_span: left.run_span.clone(),
                root: left.source_coverage_root,
                count: left.source_coverage_count,
            },
            &ShuffleSourceCoverageV1 {
                run_span: right.run_span.clone(),
                root: right.source_coverage_root,
                count: right.source_coverage_count,
            },
            limits,
        )?;
        if coverage.run_span != run_span {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let left_records = verified_shuffle_run_records(left, limits, |reference| {
            reader
                .read_verified(reference)
                .map(|object| object.bytes().to_vec())
                .map_err(|_| {
                    outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                        "shuffle producer descendant read",
                    )
                })
        })?;
        let right_records = verified_shuffle_run_records(right, limits, |reference| {
            reader
                .read_verified(reference)
                .map(|object| object.bytes().to_vec())
                .map_err(|_| {
                    outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                        "shuffle producer descendant read",
                    )
                })
        })?;
        let merged = merge_verified_shuffle_runs(expected_kind, left_records, right_records);
        if spec.phase == UnitPhase::OwnerShuffle {
            let records = merged.map(|record| match record? {
                VerifiedShuffleRecordV1::Owner(record) => Ok(record),
                VerifiedShuffleRecordV1::Bucket(_) => {
                    Err(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                        "owner shuffle producer kind",
                    ))
                }
            });
            return build_owner_shuffle_unit_artifact(
                spec, run_span, coverage, records, limits, inbox,
            );
        }
        let records = merged.map(|record| match record? {
            VerifiedShuffleRecordV1::Bucket(record) => Ok(record),
            VerifiedShuffleRecordV1::Owner(_) => {
                Err(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                    "bucket shuffle producer kind",
                ))
            }
        });
        return build_bucket_shuffle_unit_artifact(
            spec, run_span, coverage, records, limits, inbox,
        );
    };

    if let Some(records) = owner_records {
        build_owner_shuffle_unit_artifact(spec, run_span, coverage, records, limits, inbox)
    } else {
        build_bucket_shuffle_unit_artifact(
            spec,
            run_span,
            coverage,
            bucket_records.ok_or(WorkerError::UnitBindingMismatch)?,
            limits,
            inbox,
        )
    }
}

fn build_owner_shuffle_unit_artifact<I>(
    spec: &UnitSpecV1,
    run_span: outbe_ocomp_protocol::unit::CanonicalRunSpan,
    coverage: ShuffleSourceCoverageV1,
    records: I,
    limits: &SchemaLimits,
    inbox: &WorkerInbox,
) -> Result<UnitArtifactV1, WorkerError>
where
    I: IntoIterator<
        Item = Result<ProtocolContributorActionV1, outbe_ocomp_protocol::ProtocolError>,
    >,
{
    if spec.phase != UnitPhase::OwnerShuffle {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let context = ShuffleRunBuildContextV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        unit_id: spec.unit_id(limits)?,
        run_span,
        source_coverage_root: coverage.root,
        source_coverage_count: coverage.count,
    };
    let mut stage = |bytes: &[u8]| {
        inbox.stage_shuffle_object(bytes, limits).map_err(|_| {
            outbe_ocomp_protocol::ProtocolError::InvalidInvariant("shuffle object staging")
        })
    };
    let root = build_owner_shuffle_run(context, records, limits, &mut stage)?;
    finish_shuffle_unit_artifact(spec, root, limits)
}

fn build_bucket_shuffle_unit_artifact<I>(
    spec: &UnitSpecV1,
    run_span: outbe_ocomp_protocol::unit::CanonicalRunSpan,
    coverage: ShuffleSourceCoverageV1,
    records: I,
    limits: &SchemaLimits,
    inbox: &WorkerInbox,
) -> Result<UnitArtifactV1, WorkerError>
where
    I: IntoIterator<Item = Result<ShuffleBucketRecordV1, outbe_ocomp_protocol::ProtocolError>>,
{
    if spec.phase != UnitPhase::BucketShuffle {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let context = ShuffleRunBuildContextV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        unit_id: spec.unit_id(limits)?,
        run_span,
        source_coverage_root: coverage.root,
        source_coverage_count: coverage.count,
    };
    let mut stage = |bytes: &[u8]| {
        inbox.stage_shuffle_object(bytes, limits).map_err(|_| {
            outbe_ocomp_protocol::ProtocolError::InvalidInvariant("shuffle object staging")
        })
    };
    let root = build_bucket_shuffle_run(context, records, limits, &mut stage)?;
    finish_shuffle_unit_artifact(spec, root, limits)
}

fn finish_shuffle_unit_artifact(
    spec: &UnitSpecV1,
    root: ShuffleRunArtifactV1,
    limits: &SchemaLimits,
) -> Result<UnitArtifactV1, WorkerError> {
    let payload = root.encode_canonical(limits)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: root.source_coverage_root,
            output_coverage_root: root.ordered_record_root,
            source_coverage_count: root.source_coverage_count,
            output_coverage_count: root.record_count,
        },
        BoundedBytes(payload),
        limits,
    )
    .map_err(WorkerError::from)
}

fn execute_root_reduce_unit(
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
        ..
    } = authority;
    if !input_chunks.is_empty() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let phase_ordinal = unit_index
        .checked_sub(root_reduce_offset(plan, topology)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::RootReduce, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::RootReduce,
        level,
        ..
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    if level == 0 {
        return Err(WorkerError::UnsupportedPhase(UnitPhase::RootReduce));
    }

    let producer_inputs = scan_producer_inputs(spec, InputPurpose::RootSummary)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| unit_or_empty_id(input))
        .collect::<Result<Vec<_>, _>>()?;
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    if planner.root_reduce_unit_at(phase_ordinal, &producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        producer_artifacts,
        limits,
    )?;
    if resolved.len() != 2 {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let plan_hash = plan.plan_hash(limits)?;
    let mut summaries = Vec::with_capacity(2);
    for (producer, artifact) in expected.into_iter().zip(resolved) {
        match (producer, artifact) {
            (
                position @ PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    ..
                }),
                Some(artifact),
            ) => summaries.push(decode_root_reduce_producer(
                spec, position, artifact, plan_hash, limits,
            )?),
            (
                PlannedProducerV1::CanonicalEmpty {
                    purpose: InputPurpose::RootSummary,
                    padded_ordinal,
                },
                None,
            ) => summaries.push(empty_root_reduce_leaf(spec, plan_hash, padded_ordinal)?),
            _ => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    let right = summaries.pop().ok_or(WorkerError::UnitBindingMismatch)?;
    let left = summaries.pop().ok_or(WorkerError::UnitBindingMismatch)?;
    let summary = left.merge_adjacent(right)?;
    let coverage_root = summary.result_chunk_hashes.tree_root;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: summary.covered_primary_count,
            output_coverage_count: summary.covered_primary_count,
        },
        BoundedBytes(encode_root_reduce_output(
            &RootReduceOutputV1::Node { summary },
            limits,
        )?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn decode_root_reduce_producer(
    consumer: &UnitSpecV1,
    expected: PlannedProducerV1,
    artifact: &UnitArtifactV1,
    plan_hash: B256,
    limits: &SchemaLimits,
) -> Result<RootReduceSummaryV1, WorkerError> {
    let PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::RootReduce,
        level,
        index,
    }) = expected
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let output = decode_root_reduce_output(artifact.phase_payload(limits)?, limits)?;
    let summary = match (level, output) {
        (0, RootReduceOutputV1::Leaf { summary, .. }) => summary,
        (_, RootReduceOutputV1::Node { summary }) if level > 0 => summary,
        _ => return Err(WorkerError::UnitBindingMismatch),
    };
    let expected_start = index
        .checked_shl(u32::from(level))
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let header = artifact.output_header(limits)?;
    if summary.protocol_bundle_hash != consumer.protocol_bundle_hash
        || summary.job_id != consumer.job_id
        || summary.attempt != consumer.attempt
        || summary.plan_hash != plan_hash
        || summary.covered_primary_start != expected_start
        || summary.result_chunk_hashes.subtree_height != level
        || summary.result_chunk_hashes.subtree_index != index
        || header.source_coverage_root != summary.result_chunk_hashes.tree_root
        || header.output_coverage_root != summary.result_chunk_hashes.tree_root
        || header.source_coverage_count != summary.covered_primary_count
        || header.output_coverage_count != summary.covered_primary_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(summary)
}

fn empty_root_reduce_leaf(
    spec: &UnitSpecV1,
    plan_hash: B256,
    padded_ordinal: u32,
) -> Result<RootReduceSummaryV1, WorkerError> {
    Ok(RootReduceSummaryV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        plan_hash,
        covered_primary_start: padded_ordinal,
        covered_primary_count: 0,
        nod_actions: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::NodActions,
            padded_ordinal,
        )?,
        bucket_records: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::BucketRecords,
            padded_ordinal,
        )?,
        contributor_actions: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::ContributorActions,
            padded_ordinal,
        )?,
        output_manifest_entries: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::CompleteOutputManifest,
            padded_ordinal,
        )?,
        result_chunk_hashes: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::ResultChunkHashes,
            padded_ordinal,
        )?,
        tribute_count: 0,
        nod_count: 0,
        bucket_count: 0,
        contributor_count: 0,
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        nod_gratis_consumed: U256::ZERO,
        nod_cost_total: U256::ZERO,
        first_error_ordinal: None,
    })
}

fn decode_shuffle_producer_root(
    consumer: &UnitSpecV1,
    artifact: &UnitArtifactV1,
    expected_kind: ShuffleRunKindV1,
    expected_position: PlannedProducerV1,
    limits: &SchemaLimits,
) -> Result<ShuffleRunArtifactV1, WorkerError> {
    let PlannedProducerV1::Unit(PlannedUnitPositionV1::RunSpan {
        phase,
        start_run,
        end_run,
        ..
    }) = expected_position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let expected_phase = match expected_kind {
        ShuffleRunKindV1::Owner => UnitPhase::OwnerShuffle,
        ShuffleRunKindV1::Bucket => UnitPhase::BucketShuffle,
    };
    if phase != expected_phase || artifact.phase != expected_phase {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let root = ShuffleRunArtifactV1::decode_canonical(artifact.phase_payload(limits)?, limits)?;
    root.validate_root_semantics(limits)?;
    let header = artifact.output_header(limits)?;
    if root.protocol_bundle_hash != consumer.protocol_bundle_hash
        || root.job_id != consumer.job_id
        || root.attempt != consumer.attempt
        || root.unit_id != artifact.unit_id
        || root.kind != expected_kind
        || root.run_span.start_run != start_run
        || root.run_span.end_run != end_run
        || root.source_coverage_root != header.source_coverage_root
        || root.source_coverage_count != header.source_coverage_count
        || root.ordered_record_root != header.output_coverage_root
        || root.record_count != header.output_coverage_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(root)
}

fn protocol_entity_id(value: outbe_compressed_entities::EntityId36) -> ProtocolEntityId36 {
    ProtocolEntityId36(*value.as_bytes())
}

fn gratis_prefix_offset(
    plan: &PlanCommitmentV1,
    topology: LysisPlanTopologyV1,
) -> Result<u32, WorkerError> {
    plan.primary_work_unit_count
        .checked_mul(3)
        .and_then(|offset| offset.checked_add(topology.phase_unit_count(UnitPhase::FixedReduce)))
        .ok_or(WorkerError::UnitBindingMismatch)
}

fn gratis_prefix_down_offset(
    plan: &PlanCommitmentV1,
    topology: LysisPlanTopologyV1,
) -> Result<u32, WorkerError> {
    gratis_prefix_offset(plan, topology)?
        .checked_add(topology.phase_unit_count(UnitPhase::GratisPrefix))
        .ok_or(WorkerError::UnitBindingMismatch)
}

fn output_finalize_offset(
    plan: &PlanCommitmentV1,
    topology: LysisPlanTopologyV1,
) -> Result<u32, WorkerError> {
    gratis_prefix_down_offset(plan, topology)?
        .checked_add(topology.phase_unit_count(UnitPhase::GratisPrefixDown))
        .ok_or(WorkerError::UnitBindingMismatch)
}

fn owner_shuffle_offset(
    plan: &PlanCommitmentV1,
    topology: LysisPlanTopologyV1,
) -> Result<u32, WorkerError> {
    output_finalize_offset(plan, topology)?
        .checked_add(topology.phase_unit_count(UnitPhase::OutputFinalize))
        .ok_or(WorkerError::UnitBindingMismatch)
}

fn bucket_shuffle_offset(
    plan: &PlanCommitmentV1,
    topology: LysisPlanTopologyV1,
) -> Result<u32, WorkerError> {
    owner_shuffle_offset(plan, topology)?
        .checked_add(topology.phase_unit_count(UnitPhase::OwnerShuffle))
        .ok_or(WorkerError::UnitBindingMismatch)
}

fn root_reduce_offset(
    plan: &PlanCommitmentV1,
    topology: LysisPlanTopologyV1,
) -> Result<u32, WorkerError> {
    bucket_shuffle_offset(plan, topology)?
        .checked_add(topology.phase_unit_count(UnitPhase::BucketShuffle))
        .ok_or(WorkerError::UnitBindingMismatch)
}

fn scan_producer_inputs(
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

fn unit_or_empty_id(input: &CanonicalInputRefV1) -> Result<Option<B256>, WorkerError> {
    match input.source_kind {
        InputSourceKind::UnitOutput if !input.source_id.is_zero() => Ok(Some(input.source_id)),
        InputSourceKind::CanonicalEmpty => Ok(None),
        _ => Err(WorkerError::UnitBindingMismatch),
    }
}

fn resolve_scan_artifacts<'a>(
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

fn validate_scan_artifact(
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
