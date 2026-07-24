//! One socket-activated process handles one immutable work-unit request.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use alloy_primitives::B256;
use outbe_ocomp_protocol::input::{InputChunkKind, InputManifestV1};
use outbe_ocomp_protocol::unit::{InputPurpose, InputSourceKind, PlanCommitmentV1, UnitSpecV1};
use outbe_ocomp_protocol::{
    ObjectKind, RunUnitV1, SchemaLimits, UnitFinishedStatus, UnitFinishedV1, WorkerMessageKind,
};
use thiserror::Error;

use crate::bundle::PinnedProtocolBundle;
use crate::cas::{CasError, CasLimits, FilesystemCasReader};
use crate::control::{
    poc_schema_limits, require_effective_user, uid_for_user, ControlError, ControlServerSession,
    EndpointIdentity, ServerPolicy,
};
use crate::input_artifacts::{derive_input_chunk_ref, InputArtifactError};

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub expected_effective_user: String,
    pub expected_supervisor_user: String,
    pub identity: EndpointIdentity,
    pub session_generation: u64,
    pub cas_root: PathBuf,
    pub cas_limits: CasLimits,
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
}

pub fn run_one_from_inherited_fd(config: WorkerConfig) -> Result<WorkerOutcome, WorkerError> {
    require_effective_user(&config.expected_effective_user)?;
    if config.protocol_bundle.hash() != config.identity.protocol_bundle_hash {
        return Err(WorkerError::BundleIdentityMismatch);
    }
    let expected_supervisor_uid = uid_for_user(&config.expected_supervisor_user)?;
    let stream = duplicate_connected_stream(config.connection_fd)?;
    let reader = FilesystemCasReader::open(&config.cas_root, config.cas_limits)?;
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
    require_authenticated_input(
        &spec,
        InputPurpose::InputManifest,
        manifest_hash,
        manifest.tribute_count,
        request.input_manifest_ref.encoded_bytes,
    )?;
    for reference in &request.ordered_input_refs {
        if reference.expected_ocb1_kind != Some(ObjectKind::AuthenticatedInputChunkV1.tag()) {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let object = reader.read_verified(reference)?;
        let derived =
            derive_input_chunk_ref(&object, config.protocol_bundle.bundle(), &limits)?.reference;
        let purpose = match derived.kind {
            InputChunkKind::Tribute => InputPurpose::TributeStream,
            InputChunkKind::Fidelity => InputPurpose::FidelityOpenings,
            InputChunkKind::Oracle => InputPurpose::OracleOpenings,
        };
        require_authenticated_input(
            &spec,
            purpose,
            derived.semantic_digest,
            derived.record_count,
            derived.encoded_bytes,
        )?;
    }

    // OCM-11 proves the fixed process/control/CAS boundary. The deterministic
    // Lysis runner is installed by OCM-14; until then the exact admitted unit is
    // reported as a bounded failed execution, never as a fabricated artifact.
    let finished = UnitFinishedV1 {
        unit_id,
        status: UnitFinishedStatus::Failed,
        exact_staged_bytes: 0,
        transport_digest: B256::ZERO,
    };
    session.send_response(
        frame.request_id,
        WorkerMessageKind::UnitFinished as u16,
        finished.encode_body(&limits)?,
    )?;
    Ok(WorkerOutcome { unit_id })
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
