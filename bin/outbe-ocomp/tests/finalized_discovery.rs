// OCOMP-TEST-ID: OCM-DIS-001

use std::env;
use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;

use alloy_primitives::{B256, U256};
use outbe_consensus::block::ConsensusBlock;
use outbe_node::ocomp::{
    control::OcompControlServer,
    retention::{
        CandidateFinalityV1, CandidatePinV1, FinalizedInputProofSource, FinalizedJobPinV1,
        OcompRetentionCoordinator, RetentionError, RetentionStatus,
    },
};
use outbe_ocomp::{
    control::{effective_uid, poc_schema_limits, EndpointIdentity},
    supervisor::{
        DiscoveryOutcome, SupervisorDiscovery, SupervisorDiscoveryConfig, SupervisorDiscoveryError,
    },
};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    intent::{
        job_id_from_intent_id, ActivationPreconditionsV1, CertifiedParentAccountingMetadataV2,
        ContributorTargetPreconditionV1, DayType, FinalizedIntentProofV1, FrozenMetadosisValuesV1,
        JobIntentV1, MetadosisAttemptPreconditionV1, MetadosisExpectedStatus,
        NodTargetPreconditionV1, ParentProofKind, TributeInputBindingV1,
    },
};
use outbe_primitives::OutbeHeader;
use reth_ethereum::{primitives::SealedBlock, Block};

const CHILD_MODE: &str = "OCM_DIS_CHILD_MODE";
const CHILD_SOCKET: &str = "OCM_DIS_NODE_SOCKET";
const CHILD_JOURNAL: &str = "OCM_DIS_JOURNAL_ROOT";
const CHILD_NODE_UID: &str = "OCM_DIS_NODE_UID";
const CHILD_CHAIN_ID: &str = "OCM_DIS_CHAIN_ID";
const CHILD_GENESIS: &str = "OCM_DIS_GENESIS_HASH";
const CHILD_BOOT_NONCE: &str = "OCM_DIS_BOOT_NONCE";
const CHILD_BUNDLE: &str = "OCM_DIS_BUNDLE_HASH";

#[derive(Clone)]
struct FinalizedFixtureSource {
    block_hash: B256,
    pin: FinalizedJobPinV1,
    proof: FinalizedIntentProofV1,
}

impl FinalizedInputProofSource for FinalizedFixtureSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        Ok((block.block_hash() == self.block_hash).then_some(self.pin.candidate))
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        if candidate != self.pin.candidate {
            return Err(RetentionError::Source(
                "fixture received a different finalized candidate".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(self.pin))
    }

    fn build_finalized_intent_proof(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        if candidate != self.pin.candidate {
            return Err(RetentionError::Source(
                "fixture received a different proof candidate".to_owned(),
            ));
        }
        Ok(self.proof.clone())
    }
}

struct RunningControlServer {
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<std::io::Result<()>>>,
    socket: PathBuf,
}

impl RunningControlServer {
    fn start(server: Arc<OcompControlServer>, socket: &Path) -> Self {
        let listener = UnixListener::bind(socket).expect("bind real node control UDS");
        let shutdown = Arc::new(AtomicBool::new(false));
        let child_shutdown = shutdown.clone();
        let thread = thread::spawn(move || server.serve_until(listener, &child_shutdown));
        Self {
            shutdown,
            thread: Some(thread),
            socket: socket.to_path_buf(),
        }
    }

    fn stop(mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.socket);
        self.thread
            .take()
            .expect("control server thread")
            .join()
            .expect("control server did not panic")
            .expect("control listener stopped cleanly");
        fs::remove_file(&self.socket).expect("remove stopped control socket");
    }
}

impl Drop for RunningControlServer {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.shutdown.store(true, Ordering::Release);
            let _ = UnixStream::connect(&self.socket);
            let _ = thread.join();
            let _ = fs::remove_file(&self.socket);
        }
    }
}

fn block(number: u64, state_root: B256) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.state_root = state_root;
    block.header.extra_data = vec![0xD1, 0x5C].into();
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block.map_header(OutbeHeader::new)))
}

fn intent(block_number: u64, identity: EndpointIdentity) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: identity.chain_id,
        genesis_hash: identity.genesis_hash,
        fork_id: B256::repeat_byte(0x12),
        wwd: 7,
        pending_nonce: 1,
        attempt: 1,
        protocol_bundle_hash: identity.protocol_bundle_hash,
        ce_sealed_root: B256::repeat_byte(0x14),
        sealed_tribute_collection_key: B256::repeat_byte(0x15),
        sealed_tribute_collection_root: B256::repeat_byte(0x16),
        authenticated_day_count: 0,
        authenticated_day_nominal: U256::ZERO,
        pre_admission_envelope_hash: B256::repeat_byte(0x17),
        source_availability_policy_id: B256::repeat_byte(0x18),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(1_000),
            previous_vwap: U256::from(90),
            current_vwap: U256::from(100),
            gratis_demand: U256::from(25),
            gratis_supply: U256::from(20),
            lysis_budget: U256::from(300),
            auction_base: U256::from(700),
            auction_entry_price: U256::from(95),
            request_budget_split_receipt_hash: B256::repeat_byte(0x19),
        },
        logical_evaluation_height: block_number,
        logical_evaluation_time: 1_000,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: 7,
                source_generation: 1,
                collection_key: B256::repeat_byte(0x15),
                sealed_collection_root: B256::repeat_byte(0x16),
                exact_count: 0,
                exact_nominal_total: U256::ZERO,
            },
            nod: NodTargetPreconditionV1 {
                wwd: 7,
                target_generation: 1,
                namespace_root_before: B256::repeat_byte(0x1A),
                max_nod_count: 0,
            },
            contributors: ContributorTargetPreconditionV1 {
                series_id: 7,
                expected_series_version: 1,
                max_contributor_count: 0,
                max_eligible_nominal_total: U256::ZERO,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: 7,
                pending_nonce: 1,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_committee_snapshot_hash: B256::repeat_byte(0x1B),
        custody_committee_epoch_hash: None,
    }
}

fn proof(candidate: CandidatePinV1, intent: &JobIntentV1) -> FinalizedIntentProofV1 {
    let limits = poc_schema_limits();
    FinalizedIntentProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        canonical_request_header_rlp: ProofBytes(Vec::new()),
        parent_accounting: CertifiedParentAccountingMetadataV2 {
            finalized_block_number: candidate.block_number,
            finalized_block_hash: candidate.block_hash,
            finalized_epoch: 0,
            finalized_view: 0,
            parent_view: 0,
            ordered_committee: Vec::new(),
            signer_bitmap: BoundedBytes(Vec::new()),
            canonical_commonware_finalization_proof: ProofBytes(Vec::new()),
            committee_set_hash: B256::ZERO,
            vrf_material_version: 0,
            vrf_group_public_key_hash: B256::ZERO,
            proof_kind: ParentProofKind::Finalization,
            missed_proposers: Vec::new(),
        },
        historical_committee_membership_proof: ProofBytes(Vec::new()),
        canonical_job_intent: BoundedBytes(
            intent
                .encode_canonical(&limits)
                .expect("encode canonical JobIntent fixture"),
        ),
        intent_account_proof: ProofBytes(Vec::new()),
        intent_storage_proof: ProofBytes(Vec::new()),
    }
}

fn child_config() -> SupervisorDiscoveryConfig {
    SupervisorDiscoveryConfig {
        node_socket: env::var_os(CHILD_SOCKET).expect("child socket").into(),
        journal_root: env::var_os(CHILD_JOURNAL).expect("child journal").into(),
        expected_node_uid: env::var(CHILD_NODE_UID)
            .expect("child node uid")
            .parse()
            .expect("parse child node uid"),
        identity: EndpointIdentity {
            chain_id: env::var(CHILD_CHAIN_ID)
                .expect("child chain id")
                .parse()
                .expect("parse child chain id"),
            genesis_hash: env::var(CHILD_GENESIS)
                .expect("child genesis")
                .parse()
                .expect("parse child genesis"),
            boot_nonce: env::var(CHILD_BOOT_NONCE)
                .expect("child boot nonce")
                .parse()
                .expect("parse child boot nonce"),
            protocol_bundle_hash: env::var(CHILD_BUNDLE)
                .expect("child bundle")
                .parse()
                .expect("parse child bundle"),
        },
        limits: poc_schema_limits(),
    }
}

#[test]
fn ocomp_discovery_child() {
    let Some(mode) = env::var_os(CHILD_MODE) else {
        return;
    };
    let discovery = SupervisorDiscovery::open(child_config()).expect("open production journal");
    let outcome = if mode == "hint" {
        discovery.on_wake_hint()
    } else {
        discovery.reconcile_once()
    };
    match outcome {
        Ok(DiscoveryOutcome::Discovered(record)) => {
            println!(
                "OCM_DIS_OUTCOME=DISCOVERED:{}:{}",
                record.generation, record.cursor
            );
        }
        Ok(DiscoveryOutcome::NoNewJob) => println!("OCM_DIS_OUTCOME=NO_NEW_JOB"),
        Err(error) => println!("OCM_DIS_OUTCOME=ERROR:{error}"),
    }
}

fn run_supervisor_child(
    mode: &str,
    socket: &Path,
    journal: &Path,
    uid: u32,
    identity: EndpointIdentity,
) -> Output {
    Command::new(env::current_exe().expect("current integration test executable"))
        .arg("--exact")
        .arg("ocomp_discovery_child")
        .arg("--nocapture")
        .env(CHILD_MODE, mode)
        .env(CHILD_SOCKET, socket)
        .env(CHILD_JOURNAL, journal)
        .env(CHILD_NODE_UID, uid.to_string())
        .env(CHILD_CHAIN_ID, identity.chain_id.to_string())
        .env(CHILD_GENESIS, identity.genesis_hash.to_string())
        .env(CHILD_BOOT_NONCE, identity.boot_nonce.to_string())
        .env(CHILD_BUNDLE, identity.protocol_bundle_hash.to_string())
        .output()
        .expect("start real supervisor child process")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "supervisor child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("child stdout is UTF-8")
}

#[test]
fn ocm_dis_001_production_supervisor_discovers_finalized_jobs_exactly_once() {
    let temp = tempfile::tempdir().expect("OCM-DIS-001 root");
    let socket = temp.path().join("node-control.sock");
    let retention_root = temp.path().join("retention");
    let discovery_root = temp.path().join("discovery");
    let uid = effective_uid().expect("effective test uid");
    let identity = EndpointIdentity {
        chain_id: 42,
        genesis_hash: B256::repeat_byte(0x21),
        boot_nonce: B256::repeat_byte(0x22),
        protocol_bundle_hash: B256::repeat_byte(0x23),
    };
    let request = block(100, B256::repeat_byte(0x24));
    let intent = intent(request.number(), identity);
    let limits = poc_schema_limits();
    let intent_id = intent.intent_id(&limits).expect("fixture IntentId");
    let candidate = CandidatePinV1 {
        block_number: request.number(),
        block_hash: request.block_hash(),
        state_root: request.header().inner.state_root,
        intent_id,
        wwd: intent.wwd,
        ce_sealed_root: intent.ce_sealed_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        input_lease_id: intent.input_lease_id().expect("input lease id"),
    };
    let job_id = job_id_from_intent_id(intent_id, candidate.block_hash, candidate.state_root)
        .expect("fixture JobId");
    let source = Arc::new(FinalizedFixtureSource {
        block_hash: request.block_hash(),
        pin: FinalizedJobPinV1 {
            candidate,
            job_id,
            finality_recorded_height: request.number(),
            open_height: request.number() + 4,
            deadline_height: request.number() + 10,
        },
        proof: proof(candidate, &intent),
    });
    let retention = Arc::new(OcompRetentionCoordinator::open(retention_root, source));
    let server = Arc::new(
        OcompControlServer::new(retention.clone(), uid, identity, 1, limits)
            .expect("production node control server"),
    );

    // A control outage is local: finalized state advances while no supervisor
    // can connect, so discovery cannot rely on an event subscription.
    RunningControlServer::start(server.clone(), &socket).stop();
    retention
        .prepare_candidate(&request)
        .expect("candidate is durably pinned while control is down");
    retention
        .reconcile_finalized(&request)
        .expect("node finality is independent from control availability");
    assert!(matches!(retention.status(), RetentionStatus::Ready(_)));

    let running = RunningControlServer::start(server.clone(), &socket);
    let mut incompatible = identity;
    incompatible.protocol_bundle_hash = B256::repeat_byte(0xEE);
    let incompatible_output = stdout(&run_supervisor_child(
        "poll",
        &socket,
        &discovery_root,
        uid,
        incompatible,
    ));
    assert!(incompatible_output.contains("OCM_DIS_OUTCOME=ERROR:"));
    assert!(!server.readiness().is_ready());
    assert_eq!(server.readiness().incompatible_sessions(), 1);

    // No request event is delivered. Cursor reconciliation alone discovers
    // and fsyncs the exact canonical job before reporting DISCOVERED.
    let first = stdout(&run_supervisor_child(
        "poll",
        &socket,
        &discovery_root,
        uid,
        identity,
    ));
    assert!(
        first.contains("OCM_DIS_OUTCOME=DISCOVERED:1:100"),
        "got: {first}"
    );
    assert!(server.readiness().is_ready());
    let journal_path = discovery_root.join("discovery.v1");
    let first_journal = fs::read(&journal_path).expect("durable discovery journal");

    // A duplicate/reordered wakeup carries no identity and therefore cannot
    // create work after the fsync'd cursor.
    let duplicate = stdout(&run_supervisor_child(
        "hint",
        &socket,
        &discovery_root,
        uid,
        identity,
    ));
    assert!(duplicate.contains("OCM_DIS_OUTCOME=NO_NEW_JOB"));
    assert_eq!(
        fs::read(&journal_path).expect("journal after duplicate hint"),
        first_journal
    );

    // A fresh OS process resumes from the same cursor and remains exactly-once.
    let restarted = stdout(&run_supervisor_child(
        "poll",
        &socket,
        &discovery_root,
        uid,
        identity,
    ));
    assert!(restarted.contains("OCM_DIS_OUTCOME=NO_NEW_JOB"));
    assert_eq!(
        fs::read(&journal_path).expect("journal after process restart"),
        first_journal
    );
    running.stop();

    let record = SupervisorDiscovery::open(SupervisorDiscoveryConfig {
        node_socket: socket,
        journal_root: discovery_root,
        expected_node_uid: uid,
        identity,
        limits,
    })
    .expect("reopen production discovery journal")
    .current_record()
    .expect("read production discovery journal")
    .expect("one discovered job");
    assert_eq!(record.generation, 1);
    assert_eq!(record.cursor, request.number());
    assert_eq!(record.spec.summary.job_id, job_id);
    assert_eq!(
        record.spec.canonical_job_intent.0,
        intent
            .encode_canonical(&limits)
            .expect("expected canonical JobIntent")
    );

    let mut corrupted = first_journal;
    *corrupted.last_mut().expect("journal checksum byte") ^= 1;
    fs::write(&journal_path, corrupted).expect("corrupt exact journal fixture");
    assert!(matches!(
        SupervisorDiscovery::open(SupervisorDiscoveryConfig {
            node_socket: temp.path().join("unused.sock"),
            journal_root: temp.path().join("discovery"),
            expected_node_uid: uid,
            identity,
            limits,
        }),
        Err(SupervisorDiscoveryError::JournalChecksumMismatch)
    ));
}
