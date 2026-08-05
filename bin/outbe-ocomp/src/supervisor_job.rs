//! Restart-safe composition of the closed Lysis V1 supervisor pipeline.
//!
//! This module does not introduce another scheduler or computation interface.
//! It composes the already verified export, planner, one-unit worker boundary,
//! admission catalog, finalizer and node-owned result-vote attestation gate.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy_primitives::B256;
use outbe_lysis::program_v1::planner::{
    LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1,
};
use outbe_ocomp_protocol::{
    input::InputChunkKind, intent::JobIntentV1, UnitFinishedStatus, UnitFinishedV1,
    WorkerMessageKind,
};
use outbe_ocomp_protocol::{RunUnitV1, SchemaLimits};
use thiserror::Error;

use crate::admission_catalog::{AdmissionCatalogError, VerifiedAdmissionCatalog};
use crate::bundle::PinnedProtocolBundle;
use crate::cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader};
use crate::control::{ClientPolicy, ControlClientSession, EndpointIdentity};
use crate::export_binding::VerifiedExportedManifestBinding;
use crate::inbox::{WorkerInbox, WorkerInboxLimits};
use crate::input_artifacts::poc_input_list_limits;
use crate::input_ref_catalog::VerifiedInputChunkRefCatalog;
use crate::lysis_finalization::finalize_verified_lysis_v1;
use crate::lysis_plan_audit::LocalLysisPlanAuditV1;
use crate::lysis_scheduler::admit_reported_lysis_unit_v1;
use crate::supervisor::DiscoveryRecord;

const WORKER_IO_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Debug)]
pub struct SupervisorJobRunnerConfigV1 {
    pub cas_root: PathBuf,
    pub cas_limits: CasLimits,
    pub input_ref_root: PathBuf,
    pub job_root: PathBuf,
    pub worker_inbox_root: PathBuf,
    pub worker_inbox_limits: WorkerInboxLimits,
    pub worker_socket: PathBuf,
    pub identity: EndpointIdentity,
    pub protocol_bundle: PinnedProtocolBundle,
    pub limits: SchemaLimits,
    pub development_worker: Option<DevelopmentWorkerLaunchV1>,
}

#[derive(Clone, Debug)]
pub struct DevelopmentWorkerLaunchV1 {
    pub binary: PathBuf,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedSupervisorJobV1 {
    pub job_id: B256,
    pub exact_unit_count: u32,
    pub result_digest: B256,
    pub canonical_result: Vec<u8>,
}

pub struct SupervisorJobRunnerV1 {
    config: SupervisorJobRunnerConfigV1,
    cas: FilesystemCas,
    reader: FilesystemCasReader,
    next_worker_generation: AtomicU64,
}

impl SupervisorJobRunnerV1 {
    pub fn open(config: SupervisorJobRunnerConfigV1) -> Result<Self, SupervisorJobRunnerErrorV1> {
        if config.development_worker.is_some() && !cfg!(debug_assertions) {
            return Err(SupervisorJobRunnerErrorV1::Invariant(
                "development worker activation is unavailable in release builds",
            ));
        }
        let cas = stage(
            "open supervisor job CAS writer",
            FilesystemCas::open(
                &config.cas_root,
                CasWriterRole::Supervisor,
                config.cas_limits,
            ),
        )?;
        let reader = stage(
            "open supervisor job CAS reader",
            FilesystemCasReader::open(&config.cas_root, config.cas_limits),
        )?;
        Ok(Self {
            config,
            cas,
            reader,
            next_worker_generation: AtomicU64::new(1),
        })
    }

    pub fn run_to_result(
        &self,
        record: &DiscoveryRecord,
        binding: &VerifiedExportedManifestBinding,
    ) -> Result<CompletedSupervisorJobV1, SupervisorJobRunnerErrorV1> {
        let limits = &self.config.limits;
        let job_id = record.spec.summary.job_id;
        if binding.job_id() != job_id {
            return Err(SupervisorJobRunnerErrorV1::Invariant(
                "adopted export belongs to another finalized job",
            ));
        }
        let intent = stage(
            "decode finalized JobIntent",
            JobIntentV1::decode_canonical(&record.spec.canonical_job_intent.0, limits),
        )?;
        let manifest = binding.manifest();
        let manifest_ref = binding.manifest_ref();
        let job_component = hex::encode(job_id.as_slice());
        let input_refs = stage(
            "reopen exact exported input catalog",
            VerifiedInputChunkRefCatalog::reopen(
                self.config.input_ref_root.join(&job_component),
                &self.reader,
                *limits,
                poc_input_list_limits(),
            ),
        )?;
        let tribute_refs = stage(
            "read exact Tribute input references",
            input_refs.exact_cursor(),
        )?
        .filter_map(|reference| match reference {
            Ok(reference) if reference.kind == InputChunkKind::Tribute => Some(Ok(reference)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| stage_error("read exact Tribute input references", error))?;

        let planner = stage(
            "bind Lysis V1 planner",
            LysisPlannerV1::new(LysisPlannerBindingsV1 {
                protocol_bundle_hash: self.config.protocol_bundle.hash(),
                job_id,
                attempt: intent.attempt,
                input_manifest_hash: stage(
                    "hash adopted input manifest",
                    manifest.manifest_hash(limits),
                )?,
                input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
                fidelity_opening_root: manifest.fidelity_opening_root,
                oracle_opening_root: manifest.oracle_opening_root,
                wwd: manifest.wwd,
                lysis_budget: intent.frozen_metadosis_values.lysis_budget,
                logical_evaluation_time: intent.logical_evaluation_time,
                tribute_count: manifest.tribute_count,
                lysis_program_semantics_hash: self
                    .config
                    .protocol_bundle
                    .bundle()
                    .lysis_program_semantics_hash,
                planner_spec_version: self.config.protocol_bundle.bundle().planner_spec_version,
                reducer_spec_version: self.config.protocol_bundle.bundle().reducer_spec_version,
            }),
        )?;
        let plan = stage(
            "commit exact Lysis primary catalog",
            planner.commit_primary_catalog(tribute_refs, limits),
        )?;
        let plan_bytes = stage(
            "encode exact Lysis plan",
            plan.encode_canonical_record(limits),
        )?;
        let plan_ref = stage(
            "publish exact Lysis plan",
            self.cas.publish_bytes(&plan_bytes),
        )?;
        let topology = stage(
            "derive exact Lysis plan topology",
            LysisPlanTopologyV1::new(plan.primary_work_unit_count),
        )?;
        let exact_unit_count = topology.total_unit_count();

        let local_job_root = self.config.job_root.join(&job_component);
        let mut admissions = stage(
            "open exact Lysis admission catalog",
            VerifiedAdmissionCatalog::open(
                local_job_root.join("admissions"),
                &self.cas,
                &plan_ref,
                &manifest_ref,
                *limits,
            ),
        )?;
        let worker_inbox = stage(
            "open worker report inbox",
            WorkerInbox::open(
                &self.config.worker_inbox_root,
                self.config.worker_inbox_limits,
            ),
        )?;
        let replay_inbox = stage(
            "open supervisor replay inbox",
            WorkerInbox::open(
                local_job_root.join("replay-inbox"),
                self.config.worker_inbox_limits,
            ),
        )?;

        for plan_ordinal in 0..exact_unit_count {
            match admissions.read(plan_ordinal) {
                Ok(_) => continue,
                Err(AdmissionCatalogError::MissingAdmission { .. }) => {}
                Err(error) => {
                    return Err(stage_error("inspect exact Lysis admission", error));
                }
            }
            let request = {
                let audit = stage(
                    "open exact Lysis scheduling audit",
                    LocalLysisPlanAuditV1::open(
                        &admissions,
                        &input_refs,
                        &self.reader,
                        &self.config.protocol_bundle,
                        limits,
                    ),
                )?;
                stage(
                    "derive next exact Lysis worker request",
                    audit.worker_request_at(plan_ordinal),
                )?
            };
            let finished = self.execute_one_worker(&request)?;
            if finished.status != UnitFinishedStatus::Success {
                return Err(SupervisorJobRunnerErrorV1::WorkerFailed {
                    plan_ordinal,
                    unit_id: finished.unit_id,
                });
            }
            stage(
                "admit exact Lysis worker result",
                admit_reported_lysis_unit_v1(
                    plan_ordinal,
                    &finished,
                    &mut admissions,
                    &input_refs,
                    &self.config.protocol_bundle,
                    &self.reader,
                    &worker_inbox,
                    &replay_inbox,
                    &self.cas,
                    limits,
                ),
            )?;
        }

        let audit = stage(
            "cold-audit complete Lysis plan",
            LocalLysisPlanAuditV1::open(
                &admissions,
                &input_refs,
                &self.reader,
                &self.config.protocol_bundle,
                limits,
            ),
        )?;
        let finalized = stage(
            "finalize verified Lysis result",
            finalize_verified_lysis_v1(record, &audit, &self.cas, &self.reader, limits),
        )?;
        Ok(CompletedSupervisorJobV1 {
            job_id,
            exact_unit_count,
            result_digest: finalized.result_digest(),
            canonical_result: finalized.canonical_result_bytes().to_vec(),
        })
    }

    fn execute_one_worker(
        &self,
        request: &RunUnitV1,
    ) -> Result<UnitFinishedV1, SupervisorJobRunnerErrorV1> {
        let (stream, child) = match &self.config.development_worker {
            Some(development) => self.spawn_development_worker(development)?,
            None => (
                UnixStream::connect(&self.config.worker_socket)
                    .map_err(|error| stage_error("connect worker activation socket", error))?,
                None,
            ),
        };
        stream
            .set_read_timeout(Some(WORKER_IO_TIMEOUT))
            .map_err(|error| stage_error("set worker read timeout", error))?;
        stream
            .set_write_timeout(Some(WORKER_IO_TIMEOUT))
            .map_err(|error| stage_error("set worker write timeout", error))?;
        let mut session = stage(
            "authenticate one-unit worker",
            ControlClientSession::connect(
                stream,
                ClientPolicy::supervisor_to_worker(self.config.identity, self.config.limits),
            ),
        )?;
        stage("handshake one-unit worker", session.handshake())?;
        stage(
            "send exact Lysis unit",
            session.send_request(
                WorkerMessageKind::RunUnit as u16,
                stage(
                    "encode exact Lysis unit request",
                    request.encode_body(&self.config.limits),
                )?,
            ),
        )?;
        let response = stage("receive one-unit worker result", session.receive_response())?;
        if response.message_kind != WorkerMessageKind::UnitFinished as u16 {
            return Err(SupervisorJobRunnerErrorV1::UnexpectedWorkerResponse(
                response.message_kind,
            ));
        }
        let finished = stage(
            "decode one-unit worker result",
            UnitFinishedV1::decode_body(&response.body, &self.config.limits),
        )?;
        if let Some(mut child) = child {
            let status = child
                .wait()
                .map_err(|error| stage_error("wait for development worker", error))?;
            if !status.success() {
                return Err(SupervisorJobRunnerErrorV1::DevelopmentWorkerExited(
                    status.code(),
                ));
            }
        }
        Ok(finished)
    }

    fn spawn_development_worker(
        &self,
        development: &DevelopmentWorkerLaunchV1,
    ) -> Result<(UnixStream, Option<Child>), SupervisorJobRunnerErrorV1> {
        use std::fs::OpenOptions;
        use std::os::fd::OwnedFd;

        let generation = self.next_worker_generation.fetch_add(1, Ordering::Relaxed);
        if generation == 0 {
            return Err(SupervisorJobRunnerErrorV1::Invariant(
                "development worker session generation overflowed",
            ));
        }
        let (supervisor_stream, worker_stream) = UnixStream::pair()
            .map_err(|error| stage_error("create development worker socket", error))?;
        let worker_fd: OwnedFd = worker_stream.into();
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(development.root.join("worker-runs.log"))
            .map_err(|error| stage_error("open development worker log", error))?;
        let stderr = log
            .try_clone()
            .map_err(|error| stage_error("clone development worker log", error))?;
        let mut command = Command::new(&development.binary);
        command
            .arg("worker")
            .arg("--development-root")
            .arg(&development.root)
            .arg("--chain-id")
            .arg(self.config.identity.chain_id.to_string())
            .arg("--genesis-hash")
            .arg(format!("{:#x}", self.config.identity.genesis_hash))
            .arg("--boot-nonce")
            .arg(format!("{:#x}", self.config.identity.boot_nonce))
            .arg("--protocol-bundle-hash")
            .arg(format!("{:#x}", self.config.identity.protocol_bundle_hash))
            .arg("--session-generation")
            .arg(generation.to_string())
            .stdin(Stdio::from(worker_fd))
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        let child = command
            .spawn()
            .map_err(|error| stage_error("spawn development worker", error))?;
        Ok((supervisor_stream, Some(child)))
    }
}

fn stage<T, E: std::fmt::Display>(
    name: &'static str,
    result: Result<T, E>,
) -> Result<T, SupervisorJobRunnerErrorV1> {
    result.map_err(|error| stage_error(name, error))
}

fn stage_error(name: &'static str, error: impl std::fmt::Display) -> SupervisorJobRunnerErrorV1 {
    SupervisorJobRunnerErrorV1::Stage {
        name,
        detail: error.to_string(),
    }
}

#[derive(Debug, Error)]
pub enum SupervisorJobRunnerErrorV1 {
    #[error("supervisor Lysis job stage `{name}` failed: {detail}")]
    Stage { name: &'static str, detail: String },
    #[error("supervisor Lysis job invariant failed: {0}")]
    Invariant(&'static str),
    #[error("worker failed plan ordinal {plan_ordinal} ({unit_id})")]
    WorkerFailed { plan_ordinal: u32, unit_id: B256 },
    #[error("worker returned unexpected control message kind {0:#06x}")]
    UnexpectedWorkerResponse(u16),
    #[error("development worker exited unsuccessfully with code {0:?}")]
    DevelopmentWorkerExited(Option<i32>),
}
