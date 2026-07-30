// OCOMP-TEST-ID: OCM-DET-001

mod support;

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use alloy_primitives::{keccak256, Address, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_consensus::block::ConsensusBlock;
use outbe_e2e_harness::ocomp_finality_fixture::{finalized_intent_proof_fixture, fixture_league};
use outbe_lysis::program_v1::planner::{
    LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1,
};
use outbe_node::ocomp::{
    control::{OcompControlServer, OcompNodeAttestationConfig},
    retention::{
        CandidateFinalityV1, CandidatePinV1, FinalizedInputProofSource, FinalizedJobPinV1,
        OcompRetentionCoordinator, RetentionError,
    },
};
use outbe_ocomp::{
    admission_catalog::{AdmissionCatalogError, AdmissionOutcome, VerifiedAdmissionCatalog},
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::{
        effective_uid, effective_user_name, poc_schema_limits, ClientPolicy, ControlClientSession,
        EndpointIdentity,
    },
    inbox::{WorkerInbox, WorkerInboxLimits},
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    lysis_finalization::finalize_verified_lysis_v1,
    lysis_plan_audit::{ExactLysisPlanError, LocalLysisPlanAuditV1, LysisPlanAuditStepV1},
    lysis_result_catalog::{ExactLysisResultCatalogCursorV1, LysisResultCatalogStepV1},
    lysis_scheduler::admit_reported_lysis_unit_v1,
    supervisor::{
        DiscoveryRecord, SupervisorDiscovery, SupervisorDiscoveryConfig, SupervisorDiscoveryError,
    },
    worker::{run_one_from_inherited_fd, WorkerConfig},
};
use outbe_ocomp_protocol::{
    committee::{
        OcompCommitteeSnapshotV1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        OcompMemberV1, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::ProofBytes,
    hash::hash_framed,
    input::{
        materialize_authenticated_openings, CheckpointIdentityV1, InputChunkKind, InputManifestV1,
    },
    intent::{
        ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FinalizedIntentProofV1, FrozenMetadosisValuesV1, JobIntentV1,
        MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
        TributeInputBindingV1,
    },
    league_snapshot::league_snapshot_slot,
    opening::{
        partition_lysis_opening_subjects, LysisOpeningsProofV1, OpeningSubjectsV1,
        RawContractOpeningProofV1, RawStorageSlotV1,
    },
    registry::HashDomain,
    result::{LysisResultV1, ResultChunkV1},
    unit::UnitSpecV1,
    vote::ResultVoteV1,
    FinalizedJobSpecV1, FinalizedJobSummaryV1, LocalErrorCode, RunUnitV1, SchemaLimits,
    UnitFinishedStatus, UnitFinishedV1, WorkerMessageKind,
};
use outbe_oracle::oracle_opening_slot_plan_v1;
use outbe_primitives::addresses::{METADOSIS_ADDRESS, ORACLE_ADDRESS};
use tempfile::tempdir;

const CHILD_MODE: &str = "OUTBE_OCOMP_DET_WORKER_CHILD";
const CHILD_USER: &str = "OUTBE_OCOMP_DET_WORKER_USER";
const CHILD_CHAIN_ID: &str = "OUTBE_OCOMP_DET_CHAIN_ID";
const CHILD_GENESIS: &str = "OUTBE_OCOMP_DET_GENESIS";
const CHILD_BOOT_NONCE: &str = "OUTBE_OCOMP_DET_BOOT_NONCE";
const CHILD_BUNDLE: &str = "OUTBE_OCOMP_DET_BUNDLE";
const CHILD_GENERATION: &str = "OUTBE_OCOMP_DET_GENERATION";
const CHILD_CAS_ROOT: &str = "OUTBE_OCOMP_DET_CAS_ROOT";
const CHILD_CAS_OBJECT_CAP: &str = "OUTBE_OCOMP_DET_CAS_OBJECT_CAP";
const CHILD_CAS_TOTAL_CAP: &str = "OUTBE_OCOMP_DET_CAS_TOTAL_CAP";
const CHILD_INBOX_ROOT: &str = "OUTBE_OCOMP_DET_INBOX_ROOT";
const NODE_CHILD_MODE: &str = "OUTBE_OCOMP_SIG_NODE_CHILD";
const NODE_CHILD_ROOT: &str = "OUTBE_OCOMP_SIG_NODE_ROOT";
const NODE_CHILD_SOCKET: &str = "OUTBE_OCOMP_SIG_NODE_SOCKET";

const TEST_NAME: &str = "ocm_det_001_real_257_tribute_schedules_are_byte_identical";
const TRIBUTE_COUNT: u32 = 257;
const SHARD_CAP: u32 = 256;
const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: 256 * 1_048_576,
};
const INBOX_LIMITS: WorkerInboxLimits = WorkerInboxLimits {
    max_artifact_bytes: 1_048_576,
    max_total_bytes: 128 * 1_048_576,
};

#[derive(Debug)]
struct ScheduleOutcome {
    job_id: B256,
    plan_bytes: Vec<u8>,
    summary_bytes: Vec<u8>,
    result_bytes: Vec<u8>,
    result_digest: B256,
    schedule: Vec<Vec<u32>>,
}

fn endpoint_identity(boot_nonce: u8) -> EndpointIdentity {
    let limits = poc_schema_limits();
    EndpointIdentity {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        boot_nonce: B256::repeat_byte(boot_nonce),
        protocol_bundle_hash: support::protocol_bundle()
            .protocol_bundle_hash(&limits)
            .expect("deterministic fixture protocol bundle hash"),
    }
}

fn ocomp_signing_key(index: u8) -> SigningKey {
    SigningKey::from_bytes((&[index.saturating_add(1); 32]).into())
        .expect("deterministic OCOMP key")
}

fn ocomp_signature(key: &SigningKey, digest: B256) -> [u8; 64] {
    let signature: Signature = key
        .sign_prehash(digest.as_slice())
        .expect("deterministic OCOMP signature");
    signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into()
}

fn deterministic_committee() -> OcompCommitteeSnapshotV1 {
    let limits = poc_schema_limits();
    let bundle = support::protocol_bundle();
    let bundle_hash = bundle
        .protocol_bundle_hash(&limits)
        .expect("deterministic bundle hash");
    OcompCommitteeSnapshotV1 {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        fork_id: B256::repeat_byte(0x42),
        protocol_bundle_hash: bundle_hash,
        snapshot_epoch: 1,
        threshold: 3,
        ordered_members: (0..4)
            .map(|index| {
                let key = ocomp_signing_key(index);
                let public_key: [u8; 33] = key
                    .verifying_key()
                    .to_encoded_point(true)
                    .as_bytes()
                    .try_into()
                    .expect("compressed deterministic OCOMP key");
                let mut registration = OcompKeyRegistrationV1 {
                    core: OcompKeyRegistrationCoreV1 {
                        chain_id: 41,
                        genesis_hash: B256::repeat_byte(0x41),
                        fork_id: B256::repeat_byte(0x42),
                        protocol_bundle_hash: bundle_hash,
                        validator_index: index,
                        validator_identity_hash: B256::repeat_byte(0x90 + index),
                        ocomp_public_key_sec1: public_key,
                        key_epoch: 1,
                        allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
                        valid_from_height: 1,
                        valid_until_height_exclusive: 1_000,
                    },
                    proof_of_possession: [0; 64],
                };
                let pop_digest = registration
                    .proof_of_possession_digest(&limits)
                    .expect("deterministic OCOMP PoP digest");
                registration.proof_of_possession = ocomp_signature(&key, pop_digest);
                OcompMemberV1 {
                    validator_index: index,
                    validator_identity_hash: registration.core.validator_identity_hash,
                    ocomp_public_key_sec1: registration.core.ocomp_public_key_sec1,
                    key_epoch: registration.core.key_epoch,
                    allowed_purpose_bitmap: registration.core.allowed_purpose_bitmap,
                    valid_from_height: registration.core.valid_from_height,
                    valid_until_height_exclusive: registration.core.valid_until_height_exclusive,
                    proof_of_possession: registration.proof_of_possession,
                }
            })
            .collect(),
    }
}

#[derive(Clone)]
struct ExportedNodeFixtureSource {
    candidate: CandidatePinV1,
    job_id: B256,
    proof: FinalizedIntentProofV1,
}

impl FinalizedInputProofSource for ExportedNodeFixtureSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        Ok((block.block_hash() == self.candidate.block_hash).then_some(self.candidate))
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        if candidate != self.candidate {
            return Err(RetentionError::Source(
                "attestation fixture received a different candidate".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(FinalizedJobPinV1 {
            candidate,
            job_id: self.job_id,
            finality_recorded_height: candidate.block_number,
            open_height: candidate.block_number + 4,
            deadline_height: candidate.block_number + 10,
        }))
    }

    fn build_finalized_intent_proof(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        if candidate != self.candidate {
            return Err(RetentionError::Source(
                "attestation fixture received a different proof candidate".to_owned(),
            ));
        }
        Ok(self.proof.clone())
    }
}

fn exported_node_fixture() -> (ConsensusBlock, ExportedNodeFixtureSource, JobIntentV1) {
    let limits = poc_schema_limits();
    let bundle_hash = support::protocol_bundle()
        .protocol_bundle_hash(&limits)
        .expect("deterministic bundle hash");
    let day = WorldwideDay::new(20_260_725);
    let nominal_total = tributes(day)
        .iter()
        .try_fold(U256::ZERO, |total, tribute| {
            total.checked_add(tribute.nominal_amount_minor)
        })
        .expect("deterministic nominal total");
    let intent = job_intent(day, bundle_hash, nominal_total);
    let proof_fixture = finalized_intent_proof_fixture(intent.clone(), &limits);
    let candidate = CandidatePinV1 {
        block_number: proof_fixture.block.number(),
        block_hash: proof_fixture.header_hash,
        state_root: proof_fixture.state_root,
        intent_id: proof_fixture.intent_id,
        wwd: intent.wwd,
        ce_sealed_root: intent.ce_sealed_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        input_lease_id: intent.input_lease_id().expect("input lease id"),
    };
    (
        proof_fixture.block,
        ExportedNodeFixtureSource {
            candidate,
            job_id: proof_fixture.job_id,
            proof: proof_fixture.proof,
        },
        intent,
    )
}

#[test]
fn ocm_det_001_real_257_tribute_schedules_are_byte_identical() {
    if env::var_os(NODE_CHILD_MODE).is_some() {
        run_child_node();
        return;
    }
    if env::var_os(CHILD_MODE).is_some() {
        run_child_worker();
        return;
    }

    let one = run_schedule(1, 0x1357_2468);
    let two = run_schedule(2, 0x2468_1357);
    let four = run_schedule(4, 0xdead_beef);

    assert_eq!(one.job_id, two.job_id);
    assert_eq!(one.job_id, four.job_id);
    assert_eq!(one.plan_bytes, two.plan_bytes);
    assert_eq!(one.plan_bytes, four.plan_bytes);
    assert_eq!(one.summary_bytes, two.summary_bytes);
    assert_eq!(one.summary_bytes, four.summary_bytes);
    assert_eq!(one.result_bytes, two.result_bytes);
    assert_eq!(one.result_bytes, four.result_bytes);
    assert_eq!(one.result_digest, two.result_digest);
    assert_eq!(one.result_digest, four.result_digest);

    assert!(one.schedule.iter().all(|batch| batch.len() == 1));
    assert!(two.schedule.iter().any(|batch| batch.len() == 2));
    assert!(four.schedule.iter().any(|batch| batch.len() >= 3));
    assert_ne!(one.schedule, two.schedule);
    assert_ne!(two.schedule, four.schedule);

    attest_finalized_schedule_across_node_restart(&one);
}

fn attest_finalized_schedule_across_node_restart(outcome: &ScheduleOutcome) {
    let limits = poc_schema_limits();
    let root = tempdir().expect("attestation process fixture");
    let (block, source, intent) = exported_node_fixture();
    let open_height = source
        .candidate
        .block_number
        .checked_add(4)
        .expect("fixture open height");
    let deadline_height = source
        .candidate
        .block_number
        .checked_add(10)
        .expect("fixture deadline height");
    assert_eq!(source.job_id, outcome.job_id);
    let result = LysisResultV1::decode_canonical(&outcome.result_bytes, &limits)
        .expect("final Lysis result");
    assert_eq!(
        result.result_digest(&limits).expect("result digest"),
        outcome.result_digest
    );

    let retention_root = root.path().join("retention");
    let coordinator = OcompRetentionCoordinator::open(&retention_root, Arc::new(source.clone()));
    coordinator
        .reconcile_finalized(&block)
        .expect("persist finalized OCOMP authority");
    coordinator
        .record_exported(outcome.job_id, 2, 1, result.input_manifest_hash)
        .expect("persist exact exported manifest authority");
    drop(coordinator);

    let key_path = root.path().join("ocomp-key-v1.hex");
    let mut key_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
        .expect("create node OCOMP key");
    key_file
        .write_all(b"0101010101010101010101010101010101010101010101010101010101010101\n")
        .expect("write node OCOMP key");
    key_file.sync_all().expect("sync node OCOMP key");

    let first = request_node_attestation(root.path(), "first", &outcome.result_bytes)
        .expect("first process attestation");
    first
        .verify(
            &intent,
            outcome.job_id,
            &deterministic_committee(),
            open_height,
            open_height,
            deadline_height,
            &limits,
        )
        .expect("verify first process result vote");
    let replay = request_node_attestation(root.path(), "replay", &outcome.result_bytes)
        .expect("exact replay after node process restart");
    assert_eq!(replay, first);

    let mut conflicting = result;
    conflicting.roots.nod_root = B256::repeat_byte(0xe1);
    conflicting.arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &conflicting
            .arithmetic_summary()
            .encode_canonical(&limits)
            .expect("conflicting arithmetic summary"),
    )
    .expect("conflicting arithmetic commitment");
    conflicting
        .validate_semantics(&limits)
        .expect("internally valid conflicting result");
    let error = request_node_attestation(
        root.path(),
        "conflict",
        &conflicting
            .encode_canonical(&limits)
            .expect("canonical conflicting result"),
    )
    .expect_err("second digest must be refused after node restart");
    assert!(matches!(
        error,
        SupervisorDiscoveryError::RemoteRejected {
            error_code,
            retryable: false,
            ..
        } if error_code == LocalErrorCode::Conflict as u16
    ));
}

fn request_node_attestation(
    root: &Path,
    run: &str,
    canonical_result: &[u8],
) -> Result<ResultVoteV1, SupervisorDiscoveryError> {
    let socket = root.join(format!("node-{run}.sock"));
    let mut child = Command::new(env::current_exe().expect("current deterministic test binary"));
    child
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(NODE_CHILD_MODE, "1")
        .env(NODE_CHILD_ROOT, root)
        .env(NODE_CHILD_SOCKET, &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = child.spawn().expect("spawn attestation node process");
    for _ in 0..500 {
        if socket.exists() {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll attestation node") {
            panic!("attestation node exited before binding UDS: {status}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "attestation node did not bind its UDS");

    let supervisor = SupervisorDiscovery::open(SupervisorDiscoveryConfig {
        node_socket: socket,
        journal_root: root.join(format!("supervisor-{run}")),
        expected_node_uid: effective_uid().expect("effective uid"),
        identity: endpoint_identity(0x92),
        limits: poc_schema_limits(),
    })
    .expect("open supervisor attestation client");
    let response = supervisor.request_attestation(canonical_result);
    drop(supervisor);
    let output = child
        .wait_with_output()
        .expect("wait for attestation node process");
    assert!(
        output.status.success(),
        "attestation node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    response
}

fn run_child_node() {
    let root = PathBuf::from(env::var_os(NODE_CHILD_ROOT).expect("node child root"));
    let socket = PathBuf::from(env::var_os(NODE_CHILD_SOCKET).expect("node child socket"));
    let (_, source, _) = exported_node_fixture();
    let initial_height = source
        .candidate
        .block_number
        .checked_add(4)
        .expect("fixture open height");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(
        root.join("retention"),
        Arc::new(source),
    ));
    let uid = fs::metadata(&root).expect("node child root metadata").uid();
    let server = OcompControlServer::new(
        coordinator,
        uid,
        endpoint_identity(0x91),
        1,
        poc_schema_limits(),
    )
    .expect("construct node control server")
    .with_node_attestation(OcompNodeAttestationConfig {
        key_path: root.join("ocomp-key-v1.hex"),
        sign_once_root: root.join("sign-once"),
        expected_owner_uid: uid,
        validator_index: 0,
        committee: deterministic_committee(),
        initial_height,
    })
    .expect("configure closed node attestation");
    let listener = UnixListener::bind(&socket).expect("bind node attestation UDS");
    let (stream, _) = listener.accept().expect("accept supervisor attestation");
    server
        .serve_connection(stream)
        .expect("serve supervisor attestation");
    drop(listener);
    fs::remove_file(&socket).expect("remove node attestation UDS");
}

fn run_schedule(worker_count: usize, seed: u64) -> ScheduleOutcome {
    let directory = tempdir().expect("deterministic schedule fixture");
    let cas_root = directory.path().join("cas");
    let input_ref_root = directory.path().join("input-refs");
    let admission_root = directory.path().join("admissions");
    let inbox_root = directory.path().join("worker-inbox");
    let replay_inbox_root = directory.path().join("replay-inbox");
    let limits = poc_schema_limits();
    let list_limits = poc_input_list_limits();
    let bundle = support::protocol_bundle();
    let bundle_hash = bundle
        .protocol_bundle_hash(&limits)
        .expect("deterministic bundle hash");
    let pinned_bundle = PinnedProtocolBundle::decode(
        &bundle
            .encode_canonical(&limits)
            .expect("canonical deterministic bundle"),
        bundle_hash,
        &limits,
    )
    .expect("pin deterministic bundle");
    let day = WorldwideDay::new(20_260_725);
    let tributes = tributes(day);
    let nominal_total = tributes
        .iter()
        .try_fold(U256::ZERO, |total, tribute| {
            total.checked_add(tribute.nominal_amount_minor)
        })
        .expect("deterministic nominal total");
    let intent = job_intent(day, bundle_hash, nominal_total);
    let proof_fixture = finalized_intent_proof_fixture(intent.clone(), &limits);
    let job_id = proof_fixture.job_id;
    let owners = tributes
        .iter()
        .map(|tribute| tribute.owner)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let settlement_isos = [840, 978];
    let oracle_raw = raw_oracle_opening(day, proof_fixture.state_root);

    let mut fidelity_openings = Vec::new();
    let mut oracle_opening = None;
    for subjects in partition_lysis_opening_subjects(&owners, &settlement_isos, &limits)
        .expect("partition deterministic opening subjects")
    {
        let fidelity = raw_fidelity_opening(&subjects, proof_fixture.state_root, intent.wwd);
        let authenticated = materialize_authenticated_openings(
            &LysisOpeningsProofV1 {
                protocol_bundle_hash: bundle_hash,
                job_id,
                finalized_block_hash: proof_fixture.header_hash,
                finalized_state_root: proof_fixture.state_root,
                wwd: day.value(),
                subjects,
                fidelity,
                oracle: oracle_raw.clone(),
            },
            &bundle,
            &limits,
        )
        .expect("materialize deterministic openings");
        fidelity_openings.push(authenticated.fidelity);
        match &oracle_opening {
            None => oracle_opening = Some(authenticated.oracle),
            Some(existing) => assert_eq!(existing, &authenticated.oracle),
        }
    }

    let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS)
        .expect("open deterministic CAS");
    let published = publish_input_artifact_set(
        &cas,
        &input_ref_root,
        &bundle,
        InputArtifactContents {
            identity: InputArtifactIdentity {
                job_id,
                attempt: intent.attempt,
                checkpoint: CheckpointIdentityV1 {
                    finalized_block_number: proof_fixture.block.number(),
                    finalized_block_hash: proof_fixture.header_hash,
                    finalized_state_root: proof_fixture.state_root,
                    finalized_ce_root: intent.ce_sealed_root,
                    ce_schema_version: 1,
                },
                wwd: day.value(),
                sealed_tribute_collection_key: intent.sealed_tribute_collection_key,
                sealed_tribute_collection_root: intent.sealed_tribute_collection_root,
            },
            canonical_tributes: tributes
                .iter()
                .map(|tribute| encode_tribute_v1(tribute).expect("canonical deterministic Tribute"))
                .collect(),
            fidelity_openings,
            oracle_opening: oracle_opening.expect("one deterministic Oracle opening"),
        },
        &limits,
        list_limits,
    )
    .expect("publish deterministic input artifacts");
    let manifest = InputManifestV1::decode_canonical(
        cas.read_verified(&published.manifest_ref)
            .expect("reload deterministic manifest")
            .bytes(),
        &limits,
    )
    .expect("decode deterministic manifest");
    assert_eq!(manifest.tribute_count, TRIBUTE_COUNT);

    let input_refs_for_plan = VerifiedInputChunkRefCatalog::open(
        &input_ref_root,
        &cas,
        &published.manifest_ref,
        limits,
        list_limits,
    )
    .expect("open deterministic input-ref catalog");
    let tribute_refs = input_refs_for_plan
        .exact_cursor()
        .expect("open exact deterministic input refs")
        .filter_map(|reference| match reference {
            Ok(reference) if reference.kind == InputChunkKind::Tribute => Some(reference),
            Ok(_) => None,
            Err(error) => panic!("read exact deterministic input refs: {error}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(tribute_refs.len(), 2);
    drop(input_refs_for_plan);

    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: bundle_hash,
        job_id,
        attempt: intent.attempt,
        input_manifest_hash: manifest
            .manifest_hash(&limits)
            .expect("deterministic manifest hash"),
        input_manifest_encoded_bytes: published.manifest_ref.encoded_bytes,
        fidelity_opening_root: manifest.fidelity_opening_root,
        oracle_opening_root: manifest.oracle_opening_root,
        wwd: manifest.wwd,
        lysis_budget: intent.frozen_metadosis_values.lysis_budget,
        logical_evaluation_time: intent.logical_evaluation_time,
        tribute_count: manifest.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: bundle.planner_spec_version,
        reducer_spec_version: bundle.reducer_spec_version,
    })
    .expect("build deterministic planner");
    let plan = planner
        .commit_primary_catalog(tribute_refs, &limits)
        .expect("commit deterministic primary catalog");
    assert_eq!(plan.max_tributes_per_work_shard, SHARD_CAP);
    assert_eq!(plan.primary_work_unit_count, 2);
    let plan_bytes = plan
        .encode_canonical_record(&limits)
        .expect("canonical deterministic plan");
    let plan_ref = cas
        .publish_bytes(&plan_bytes)
        .expect("publish deterministic plan");
    let topology =
        LysisPlanTopologyV1::new(plan.primary_work_unit_count).expect("deterministic topology");
    let total_units = topology.total_unit_count();

    let discovery = DiscoveryRecord {
        generation: 1,
        cursor: 1,
        spec: FinalizedJobSpecV1 {
            summary: FinalizedJobSummaryV1 {
                cursor: 1,
                job_id,
                intent_id: proof_fixture.intent_id,
                finalized_block_hash: proof_fixture.header_hash,
                finalized_state_root: proof_fixture.state_root,
                protocol_bundle_hash: bundle_hash,
            },
            canonical_job_intent: proof_fixture.proof.canonical_job_intent.clone(),
        },
    };
    let reader =
        FilesystemCasReader::open(&cas_root, CAS_LIMITS).expect("open deterministic CAS reader");
    let input_refs =
        VerifiedInputChunkRefCatalog::reopen(&input_ref_root, &reader, limits, list_limits)
            .expect("reopen deterministic input refs");
    let mut admissions = VerifiedAdmissionCatalog::open(
        &admission_root,
        &cas,
        &plan_ref,
        &published.manifest_ref,
        limits,
    )
    .expect("open deterministic admissions");
    let worker_inbox =
        WorkerInbox::open(&inbox_root, INBOX_LIMITS).expect("open deterministic worker inbox");
    let replay_inbox = WorkerInbox::open(&replay_inbox_root, INBOX_LIMITS)
        .expect("open deterministic replay inbox");
    let user = effective_user_name().expect("deterministic worker user");
    let uid = effective_uid().expect("deterministic worker uid");

    let mut completed = vec![false; total_units as usize];
    let mut schedule = Vec::new();
    let mut round = 0_u64;
    let mut restart_exercised = false;
    let mut retry_exercised = false;
    let mut missing_shard_exercised = false;

    while completed.iter().any(|done| !done) {
        let mut ready = {
            let audit = LocalLysisPlanAuditV1::open(
                &admissions,
                &input_refs,
                &reader,
                &pinned_bundle,
                &limits,
            )
            .expect("open deterministic scheduler audit");
            (0..total_units)
                .filter(|ordinal| !completed[*ordinal as usize])
                .filter_map(|ordinal| match audit.worker_request_at(ordinal) {
                    Ok(request) => Some((ordinal, request)),
                    Err(ExactLysisPlanError::Admission(
                        AdmissionCatalogError::MissingAdmission { .. },
                    )) => None,
                    Err(error) => panic!("derive ready deterministic unit {ordinal}: {error}"),
                })
                .collect::<Vec<_>>()
        };
        assert!(
            !ready.is_empty(),
            "incomplete deterministic plan must have a ready unit"
        );
        ready.sort_by_key(|(ordinal, _)| schedule_key(seed, round, *ordinal));
        ready.truncate(worker_count.min(ready.len()));
        schedule.push(ready.iter().map(|(ordinal, _)| *ordinal).collect());

        let mut reports = std::thread::scope(|scope| {
            let handles = ready
                .into_iter()
                .enumerate()
                .map(|(slot, (ordinal, request))| {
                    let user = &user;
                    let cas_root = &cas_root;
                    let inbox_root = &inbox_root;
                    (
                        ordinal,
                        request.clone(),
                        scope.spawn(move || {
                            execute_worker_request(
                                10_000 + round * 16 + slot as u64,
                                0x20_u8.wrapping_add(ordinal as u8),
                                user,
                                uid,
                                cas_root,
                                inbox_root,
                                &request,
                                limits,
                            )
                        }),
                    )
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|(ordinal, request, handle)| {
                    (
                        ordinal,
                        request,
                        handle.join().expect("deterministic worker thread"),
                    )
                })
                .collect::<Vec<_>>()
        });
        reports.sort_by_key(|(ordinal, _, _)| schedule_key(seed.rotate_left(17), round, *ordinal));

        for (ordinal, request, finished) in reports {
            let retry = if !retry_exercised {
                interrupt_worker_request(
                    20_000 + round,
                    0x90_u8.wrapping_add(ordinal as u8),
                    &user,
                    uid,
                    &cas_root,
                    &inbox_root,
                    &request,
                    limits,
                );
                let retry = execute_worker_request(
                    30_000 + round,
                    0xa0_u8.wrapping_add(ordinal as u8),
                    &user,
                    uid,
                    &cas_root,
                    &inbox_root,
                    &request,
                    limits,
                );
                assert_eq!(finished, retry);
                retry_exercised = true;
                Some(retry)
            } else {
                None
            };

            if !restart_exercised {
                let staged = worker_inbox
                    .read_reported(
                        finished.unit_id,
                        finished.exact_staged_bytes,
                        finished.transport_digest,
                    )
                    .expect("read CAS-before-journal artifact");
                let prepublished = cas
                    .publish_bytes(staged.bytes())
                    .expect("publish before admission journal");
                assert_eq!(prepublished.transport_digest, finished.transport_digest);
                drop(admissions);
                admissions = VerifiedAdmissionCatalog::open(
                    &admission_root,
                    &cas,
                    &plan_ref,
                    &published.manifest_ref,
                    limits,
                )
                .expect("restart CAS-before-journal admissions");
                restart_exercised = true;
            }

            let admitted = admit_reported_lysis_unit_v1(
                ordinal,
                &finished,
                &mut admissions,
                &input_refs,
                &pinned_bundle,
                &reader,
                &worker_inbox,
                &replay_inbox,
                &cas,
                &limits,
            )
            .unwrap_or_else(|error| panic!("admit deterministic unit {ordinal}: {error}"));
            assert!(matches!(
                admitted.admission,
                AdmissionOutcome::NewlyAdmitted | AdmissionOutcome::ExactReplay
            ));
            if let Some(retry) = retry {
                let replayed = admit_reported_lysis_unit_v1(
                    ordinal,
                    &retry,
                    &mut admissions,
                    &input_refs,
                    &pinned_bundle,
                    &reader,
                    &worker_inbox,
                    &replay_inbox,
                    &cas,
                    &limits,
                )
                .unwrap_or_else(|error| {
                    panic!("replay exact deterministic unit {ordinal}: {error}")
                });
                assert_eq!(replayed.admission, AdmissionOutcome::ExactReplay);
            }
            completed[ordinal as usize] = true;

            if !missing_shard_exercised {
                let incomplete = LocalLysisPlanAuditV1::open(
                    &admissions,
                    &input_refs,
                    &reader,
                    &pinned_bundle,
                    &limits,
                )
                .expect("open incomplete deterministic audit");
                assert!(
                    finalize_verified_lysis_v1(&discovery, &incomplete, &cas, &reader, &limits)
                        .is_err(),
                    "one admitted primary shard must not produce a final result"
                );
                missing_shard_exercised = true;
            }
        }
        round = round.checked_add(1).expect("deterministic schedule round");
    }

    assert!(restart_exercised);
    assert!(retry_exercised);
    assert!(missing_shard_exercised);

    drop(admissions);
    drop(input_refs);
    drop(reader);
    drop(cas);

    let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS)
        .expect("restart CAS before deterministic finalization");
    let reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS)
        .expect("restart CAS reader before deterministic finalization");
    let input_refs =
        VerifiedInputChunkRefCatalog::reopen(&input_ref_root, &reader, limits, list_limits)
            .expect("restart input refs before deterministic finalization");
    let admissions = VerifiedAdmissionCatalog::open(
        &admission_root,
        &cas,
        &plan_ref,
        &published.manifest_ref,
        limits,
    )
    .expect("restart journal-before-finalize admissions");
    let audit =
        LocalLysisPlanAuditV1::open(&admissions, &input_refs, &reader, &pinned_bundle, &limits)
            .expect("cold-open complete deterministic audit");
    let summary_bytes = audit
        .audit_cursor()
        .expect("open complete deterministic audit cursor")
        .filter_map(
            |step| match step.expect("read complete deterministic audit cursor") {
                LysisPlanAuditStepV1::Artifact(artifact)
                    if artifact.plan_ordinal() == total_units - 1 =>
                {
                    Some(
                        artifact
                            .artifact()
                            .phase_payload(&limits)
                            .expect("open final deterministic summary")
                            .to_vec(),
                    )
                }
                _ => None,
            },
        )
        .next()
        .expect("reload final deterministic reduction");

    let chunks = ExactLysisResultCatalogCursorV1::open(&audit)
        .expect("open deterministic result cursor")
        .filter_map(
            |step| match step.expect("read deterministic result cursor") {
                LysisResultCatalogStepV1::Chunk(chunk) => Some(
                    ResultChunkV1::decode_canonical(chunk.canonical_chunk_bytes(), &limits)
                        .expect("decode deterministic result chunk"),
                ),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chunk_ordinal, 0);
    assert_eq!(chunks[0].ordered_nod_actions.len(), SHARD_CAP as usize);
    assert_eq!(chunks[1].chunk_ordinal, 1);
    assert_eq!(chunks[1].ordered_nod_actions.len(), 1);

    let finalized = finalize_verified_lysis_v1(&discovery, &audit, &cas, &reader, &limits)
        .expect("finalize cold-reloaded deterministic Lysis result");
    ScheduleOutcome {
        job_id,
        plan_bytes,
        summary_bytes,
        result_bytes: finalized.canonical_result_bytes().to_vec(),
        result_digest: finalized.result_digest(),
        schedule,
    }
}

fn tributes(day: WorldwideDay) -> Vec<TributeBodyV1> {
    let mut tributes = (0..TRIBUTE_COUNT)
        .map(|index| {
            let mut owner_bytes = [0_u8; 20];
            owner_bytes[16..].copy_from_slice(&(index + 1).to_be_bytes());
            let owner = Address::from(owner_bytes);
            TributeBodyV1 {
                tribute_id: derive_poseidon_entity_id(owner, day)
                    .expect("deterministic Tribute identity"),
                owner,
                worldwide_day: day,
                issuance_amount_minor: U256::from(1),
                issuance_currency: 840,
                nominal_amount_minor: U256::from(1),
                reference_currency: 978,
                tribute_price_minor: U256::from(1),
                exclude_from_intex_issuance: index % 5 == 0,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|tribute| tribute.tribute_id);
    tributes
}

fn raw_fidelity_opening(
    subjects: &OpeningSubjectsV1,
    finalized_state_root: B256,
    wwd: u32,
) -> RawContractOpeningProofV1 {
    // One per-owner league word in Metadosis storage, in owner order (the node's
    // canonical slot plan). `fixture_league` fabricates an opaque valid value;
    // the real league derivation lives in `outbe_fidelity`.
    let ordered_slots = subjects
        .owners
        .iter()
        .enumerate()
        .map(|(owner_index, owner)| RawStorageSlotV1 {
            slot: league_snapshot_slot(wwd, *owner),
            value: U256::from(fixture_league(owner_index)),
        })
        .collect::<Vec<_>>();
    RawContractOpeningProofV1 {
        contract_address: METADOSIS_ADDRESS,
        state_root: finalized_state_root,
        ordered_slots,
        account_proof: ProofBytes(vec![0xa1]),
        storage_proof: ProofBytes(vec![0xb1]),
    }
}

fn raw_oracle_opening(day: WorldwideDay, finalized_state_root: B256) -> RawContractOpeningProofV1 {
    let usd_pair = keccak256(b"USD/COEN");
    let eur_pair = keccak256(b"EUR/COEN");
    let plan = oracle_opening_slot_plan_v1(day, &[(840, usd_pair), (978, eur_pair)], 2, 0, 0)
        .expect("deterministic Oracle slot plan");
    let scale = U256::from(1_000_000_000_000_000_000_u64);
    let values = [
        U256::from_be_bytes(keccak256(b"USD").0),
        U256::from_be_bytes(usd_pair.0),
        U256::from(1),
        U256::from_be_bytes(keccak256(b"EUR").0),
        U256::from_be_bytes(eur_pair.0),
        U256::from(2),
        U256::from(1),
        U256::from(2),
        U256::from(1),
        scale,
        U256::from(2),
        scale * U256::from(2),
        U256::ZERO,
        U256::ZERO,
    ];
    assert_eq!(plan.slots.len(), values.len());
    RawContractOpeningProofV1 {
        contract_address: ORACLE_ADDRESS,
        state_root: finalized_state_root,
        ordered_slots: plan
            .slots
            .into_iter()
            .zip(values)
            .map(|(slot, value)| RawStorageSlotV1 { slot, value })
            .collect(),
        account_proof: ProofBytes(vec![0xa2]),
        storage_proof: ProofBytes(vec![0xb2]),
    }
}

fn job_intent(day: WorldwideDay, protocol_bundle_hash: B256, nominal_total: U256) -> JobIntentV1 {
    let collection_key = B256::repeat_byte(0x51);
    let collection_root = B256::repeat_byte(0x52);
    let result_committee_snapshot_hash = deterministic_committee()
        .snapshot_hash(&poc_schema_limits())
        .expect("deterministic committee snapshot hash");
    JobIntentV1 {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        fork_id: B256::repeat_byte(0x42),
        wwd: day.value(),
        pending_nonce: 1,
        attempt: 1,
        protocol_bundle_hash,
        ce_sealed_root: B256::repeat_byte(0x50),
        sealed_tribute_collection_key: collection_key,
        sealed_tribute_collection_root: collection_root,
        authenticated_day_count: TRIBUTE_COUNT,
        authenticated_day_nominal: nominal_total,
        pre_admission_envelope_hash: B256::repeat_byte(0x53),
        source_availability_policy_id: B256::repeat_byte(0x54),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(1_000_700),
            previous_vwap: U256::from(90),
            current_vwap: U256::from(100),
            gratis_demand: U256::from(25),
            gratis_supply: U256::from(20),
            lysis_budget: U256::from(1_000_000),
            auction_base: U256::from(700),
            auction_entry_price: U256::from(95),
            request_budget_split_receipt_hash: B256::repeat_byte(0x55),
        },
        logical_evaluation_height: 1,
        logical_evaluation_time: 1_784_765_900,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: day.value(),
                source_generation: 1,
                collection_key,
                sealed_collection_root: collection_root,
                exact_count: TRIBUTE_COUNT,
                exact_nominal_total: nominal_total,
            },
            nod: NodTargetPreconditionV1 {
                wwd: day.value(),
                target_generation: 1,
                namespace_root_before: B256::repeat_byte(0x56),
                max_nod_count: TRIBUTE_COUNT,
            },
            contributors: ContributorTargetPreconditionV1 {
                series_id: day.value(),
                expected_series_version: 1,
                max_contributor_count: TRIBUTE_COUNT,
                max_eligible_nominal_total: nominal_total,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: day.value(),
                pending_nonce: 1,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_committee_snapshot_hash,
        custody_committee_epoch_hash: None,
    }
}

fn schedule_key(seed: u64, round: u64, ordinal: u32) -> u64 {
    let mut value = seed
        ^ round.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ u64::from(ordinal).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[allow(clippy::too_many_arguments)]
fn launch_worker(
    generation: u64,
    boot: u8,
    user: &str,
    uid: u32,
    cas_root: &Path,
    inbox_root: &Path,
    limits: SchemaLimits,
) -> (Child, ControlClientSession) {
    let (parent_stream, child_stream) = UnixStream::pair().expect("deterministic worker socket");
    let child_fd: OwnedFd = child_stream.into();
    let worker_identity = endpoint_identity(boot);
    let mut command = Command::new(env::current_exe().expect("current deterministic test binary"));
    command
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MODE, "1")
        .env(CHILD_USER, user)
        .env(CHILD_CHAIN_ID, worker_identity.chain_id.to_string())
        .env(
            CHILD_GENESIS,
            format!("{:#x}", worker_identity.genesis_hash),
        )
        .env(
            CHILD_BOOT_NONCE,
            format!("{:#x}", worker_identity.boot_nonce),
        )
        .env(
            CHILD_BUNDLE,
            format!("{:#x}", worker_identity.protocol_bundle_hash),
        )
        .env(CHILD_GENERATION, generation.to_string())
        .env(CHILD_CAS_ROOT, cas_root)
        .env(
            CHILD_CAS_OBJECT_CAP,
            CAS_LIMITS.max_object_bytes.to_string(),
        )
        .env(CHILD_CAS_TOTAL_CAP, CAS_LIMITS.max_total_bytes.to_string())
        .env(CHILD_INBOX_ROOT, inbox_root)
        .stdin(Stdio::from(child_fd))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn deterministic worker");
    drop(command);

    let client_identity = EndpointIdentity {
        boot_nonce: B256::repeat_byte(boot.wrapping_add(1)),
        ..worker_identity
    };
    let mut client = ControlClientSession::connect(
        parent_stream,
        ClientPolicy::supervisor_to_worker(uid, client_identity, limits),
    )
    .expect("connect deterministic worker");
    client.handshake().expect("handshake deterministic worker");
    (child, client)
}

#[allow(clippy::too_many_arguments)]
fn execute_worker_request(
    generation: u64,
    boot: u8,
    user: &str,
    uid: u32,
    cas_root: &Path,
    inbox_root: &Path,
    request: &RunUnitV1,
    limits: SchemaLimits,
) -> UnitFinishedV1 {
    let (child, mut client) =
        launch_worker(generation, boot, user, uid, cas_root, inbox_root, limits);
    client
        .send_request(
            WorkerMessageKind::RunUnit as u16,
            request
                .encode_body(&limits)
                .expect("encode deterministic RunUnit"),
        )
        .expect("send deterministic RunUnit");
    let output = child
        .wait_with_output()
        .expect("wait for deterministic worker");
    assert!(
        output.status.success(),
        "deterministic worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frame = client
        .receive_response()
        .expect("receive deterministic worker response");
    let finished =
        UnitFinishedV1::decode_body(&frame.body, &limits).expect("decode deterministic completion");
    if finished.status != UnitFinishedStatus::Success {
        let spec = UnitSpecV1::decode_canonical(&request.canonical_unit_spec.0, &limits)
            .expect("decode failed deterministic UnitSpec");
        panic!(
            "deterministic worker failed unit_index={} phase={:?} unit_id={}; stderr={}",
            request.unit_index,
            spec.phase,
            finished.unit_id,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    finished
}

#[allow(clippy::too_many_arguments)]
fn interrupt_worker_request(
    generation: u64,
    boot: u8,
    user: &str,
    uid: u32,
    cas_root: &Path,
    inbox_root: &Path,
    request: &RunUnitV1,
    limits: SchemaLimits,
) {
    let (mut child, mut client) =
        launch_worker(generation, boot, user, uid, cas_root, inbox_root, limits);
    client
        .send_request(
            WorkerMessageKind::RunUnit as u16,
            request
                .encode_body(&limits)
                .expect("encode interrupted deterministic RunUnit"),
        )
        .expect("send interrupted deterministic RunUnit");
    let _ = child.kill();
    let _ = child.wait();
}

fn run_child_worker() {
    let parse_u64 = |name: &str| {
        env::var(name)
            .unwrap_or_else(|_| panic!("missing {name}"))
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("invalid {name}"))
    };
    let parse_b256 = |name: &str| {
        env::var(name)
            .unwrap_or_else(|_| panic!("missing {name}"))
            .parse::<B256>()
            .unwrap_or_else(|_| panic!("invalid {name}"))
    };
    let user = env::var(CHILD_USER).expect("deterministic worker child user");
    let uid = outbe_ocomp::control::uid_for_user(&user).expect("deterministic worker child uid");
    let limits = poc_schema_limits();
    let expected_bundle_hash = parse_b256(CHILD_BUNDLE);
    let canonical_bundle = support::protocol_bundle()
        .encode_canonical(&limits)
        .expect("canonical deterministic child bundle");
    run_one_from_inherited_fd(WorkerConfig {
        expected_effective_uid: uid,
        expected_supervisor_uid: uid,
        identity: EndpointIdentity {
            chain_id: parse_u64(CHILD_CHAIN_ID),
            genesis_hash: parse_b256(CHILD_GENESIS),
            boot_nonce: parse_b256(CHILD_BOOT_NONCE),
            protocol_bundle_hash: expected_bundle_hash,
        },
        session_generation: parse_u64(CHILD_GENERATION),
        cas_root: PathBuf::from(env::var_os(CHILD_CAS_ROOT).expect("deterministic child CAS root")),
        cas_limits: CAS_LIMITS,
        inbox_root: PathBuf::from(
            env::var_os(CHILD_INBOX_ROOT).expect("deterministic child inbox root"),
        ),
        inbox_limits: INBOX_LIMITS,
        connection_fd: 0,
        protocol_bundle: PinnedProtocolBundle::decode(
            &canonical_bundle,
            expected_bundle_hash,
            &limits,
        )
        .expect("pin deterministic child bundle"),
    })
    .expect("production deterministic worker function");
}
