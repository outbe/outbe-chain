// OCOMP-TEST-ID: OCM-EXP-001

mod support;

use std::collections::BTreeSet;
use std::env;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_tribute_v1, partition_collection_key,
    sealed_root, AuthenticatedExportView, CandidateCacheLimits, CeMdbx, CeMdbxReadOnly,
    CompressedTreeService, EntityRef, EnvironmentIdentity, ExactParentIdentity, ExportLeaseOffer,
    ExportLeaseStatus, FinalLeafMutation, FinalizedMarker, PartitionRef, TributeBodyV1,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_consensus::block::ConsensusBlock;
use outbe_e2e_harness::{
    mongo_fixture::ManagedMongoReplicaSet,
    ocomp_finality_fixture::{finalized_lysis_input_fixture, LysisOpeningProvider},
};
use outbe_node::ocomp::{
    control::OcompControlServer,
    retention::{
        CandidateFinalityV1, CandidatePinV1, FinalizedInputProofSource, FinalizedJobPinV1,
        OcompRetentionCoordinator, RetentionError,
    },
    snapshot_control::{ProjectionContainmentAuthority, RethProjectionContainmentAuthority},
    verify_lysis_openings,
};
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas},
    control::{effective_uid, poc_schema_limits, EndpointIdentity},
    exporter::FinalizedTributeSource,
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    snapshot_client::{SnapshotExporterNodeClient, SnapshotExporterNodeConfig},
};
use outbe_ocomp_protocol::local_control::{ClientPolicy, ControlClientSession};
use outbe_ocomp_protocol::{
    input::materialize_authenticated_openings,
    intent::{
        ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        ExpectedFinalizedIntentBindingV1, FinalizedIntentProofV1, FrozenMetadosisValuesV1,
        JobIntentV1, MetadosisAttemptPreconditionV1, MetadosisExpectedStatus,
        NodTargetPreconditionV1, TributeInputBindingV1,
    },
    opening::{partition_lysis_opening_subjects, LysisOpeningsProofV1, OpeningSubjectsV1},
    CommitSnapshotExportV1, ListFinalizedJobsResponseV1, ListFinalizedJobsV1, NodeMessageKind,
    OpenSnapshotLeaseV1, RenewSnapshotLeaseV1, SnapshotHandoffV1, MAX_FINALIZED_JOBS_PER_RESPONSE,
};
use outbe_offchain_data::{
    FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
};
use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
use outbe_primitives::{addresses::TRIBUTE_ADDRESS, projection::ProjectionCheckpoint};
use outbe_tribute::{
    from_canonical_body, precompile::ITribute, RetainedTributePin, TributeRepositoryWriter,
};

const CHILD_MODE: &str = "OCM_EXP_CHILD_MODE";
const CHILD_DATADIR: &str = "OCM_EXP_CE_DATADIR";
const CHILD_NODE_SOCKET: &str = "OCM_EXP_NODE_SOCKET";
const CHILD_NODE_UID: &str = "OCM_EXP_NODE_UID";
const CHILD_SKIP_ACK: &str = "OCM_EXP_SKIP_ACK";
const CHILD_MONGO_URI: &str = "OCM_EXP_MONGO_URI";
const CHILD_MONGO_DATABASE: &str = "OCM_EXP_MONGO_DATABASE";
const VENDOR_REVISION: &str = "ad555350c866b2265d87d2d7fbd146fbc918bfe5";
const FORK_ID: B256 = B256::repeat_byte(0x14);

#[test]
fn ocm_exp_001_child() {
    if env::var_os(CHILD_MODE).as_deref() != Some("open-exact".as_ref()) {
        return;
    }

    let datadir = env::var_os(CHILD_DATADIR).expect("child CE datadir");
    let limits = poc_schema_limits();
    let mut node = SnapshotExporterNodeClient::connect(&SnapshotExporterNodeConfig {
        node_socket: env::var_os(CHILD_NODE_SOCKET)
            .expect("child snapshot UDS")
            .into(),
        expected_node_uid: env::var(CHILD_NODE_UID)
            .expect("child node uid")
            .parse()
            .expect("parse node uid"),
        identity: endpoint_identity(),
        limits,
    })
    .expect("connect production snapshot-exporter node endpoint");
    let listing = node.list(0).expect("list node-owned snapshot handoff");
    assert_eq!(listing.handoffs.len(), 1, "one PoC snapshot handoff");
    let listed = listing
        .handoffs
        .into_iter()
        .next()
        .expect("snapshot handoff");
    let handoff = node
        .get(listed.job_id, listed.lease_generation)
        .expect("read exact snapshot handoff");
    assert_eq!(handoff, listed);
    let verified = node
        .authenticate_handoff(
            &handoff,
            ExpectedFinalizedIntentBindingV1 {
                chain_id: environment().chain_id,
                genesis_hash: environment().genesis_hash,
                fork_id: FORK_ID,
                protocol_bundle_hash: endpoint_identity().protocol_bundle_hash,
            },
        )
        .expect("authenticate q=3/4 finality, historical committee and intent MPT");
    let day = WorldwideDay::new(verified.intent.wwd);
    let expected_root = verified.intent.sealed_tribute_collection_root;
    let expected_count = verified.intent.authenticated_day_count;
    let expected_nominal = verified.intent.authenticated_day_nominal;
    let offer = ExportLeaseOffer::decode_fixed(&handoff.canonical_lease_offer.0)
        .expect("decode node-minted lease");
    assert_eq!(offer.generation(), handoff.lease_generation);
    assert_eq!(
        offer.identity().block_number,
        handoff.checkpoint.finalized_block_number
    );
    assert_eq!(
        offer.identity().block_hash,
        handoff.checkpoint.finalized_block_hash
    );
    assert_eq!(offer.identity().root, handoff.checkpoint.finalized_ce_root);

    let store = CeMdbxReadOnly::open(datadir.as_ref(), environment())
        .expect("open production read-only CE");
    let catalog = store
        .open_exact(offer.identity())
        .expect("open exact finalized CE snapshot");
    let acknowledgement = offer
        .confirm_open(&catalog)
        .expect("ack only the exact opened snapshot");
    if env::var_os(CHILD_SKIP_ACK).is_none() {
        let opened = node
            .acknowledge_open(RenewSnapshotLeaseV1 {
                job_id: handoff.job_id,
                lease_generation: handoff.lease_generation,
                canonical_open_ack: outbe_ocomp_protocol::common::BoundedBytes(
                    acknowledgement.encode_fixed().to_vec(),
                ),
            })
            .expect("acknowledge exact opened snapshot over node UDS");
        assert_eq!(opened.job_id, handoff.job_id);
        assert_eq!(opened.lease_generation, handoff.lease_generation);
    }
    let export = AuthenticatedExportView::new(catalog).expect("open authenticated export view");
    let closure = export
        .close_tribute_partition(
            day,
            verified.intent.sealed_tribute_collection_key,
            expected_root,
            expected_count,
        )
        .expect("close exact Tribute partition");

    let storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri: env::var(CHILD_MONGO_URI).expect("child Mongo URI"),
            database: env::var(CHILD_MONGO_DATABASE).expect("child Mongo database"),
        })
        .expect("connect existing production Mongo storage"),
    );
    let pin = RetainedTributePin {
        job_id: handoff.job_id,
        worldwide_day: day,
    };
    let source = FinalizedTributeSource::new(storage, 1).expect("open bounded Tribute source");
    let projection = source
        .projection_state(ProjectionConfig {
            chain_id: environment().chain_id,
            genesis_hash: environment().genesis_hash,
            start_block: 1,
        })
        .expect("read projection state without writer capability")
        .expect("projection state exists");
    let projection_checkpoint = projection.checkpoint.expect("projection checkpoint exists");
    let contained = node
        .require_projection_contains(
            &handoff,
            projection_checkpoint.block_number,
            projection_checkpoint.block_hash,
        )
        .expect("node proves projection checkpoint contains the finalized job");
    assert_eq!(contained.job_id, handoff.job_id);
    assert_eq!(contained.lease_generation, handoff.lease_generation);
    let mut stream = source
        .stream(pin, &closure, expected_nominal)
        .expect("open authenticated Tribute stream");
    let mut ordered_ids = Vec::new();
    let mut canonical_tributes = Vec::new();
    let mut owners = BTreeSet::new();
    let mut settlement_isos = BTreeSet::new();
    settlement_isos.insert(840);
    while let Some(record) = stream.next_record().expect("read authenticated body") {
        ordered_ids.push(hex::encode(record.tribute_id.as_bytes()));
        owners.insert(record.body.owner);
        settlement_isos.insert(record.body.reference_currency);
        canonical_tributes.push(record.canonical_body);
    }
    let summary = stream.finish().expect("close body count and nominal");
    let owner_set = owners.into_iter().collect::<Vec<_>>();
    let iso_set = settlement_isos.into_iter().collect::<Vec<_>>();
    let subjects =
        partition_lysis_opening_subjects(&owner_set, &iso_set, &limits).expect("partition owners");
    let mut fidelity_openings = Vec::new();
    let mut oracle_opening = None;
    let mut fidelity_slot_count = 0_usize;
    let mut oracle_slot_count = 0_usize;
    for subject_batch in &subjects {
        let openings = node
            .lysis_openings(handoff.job_id, subject_batch.clone())
            .expect("request one bounded exact-block Fidelity and Oracle opening batch");
        verify_lysis_openings(&openings, &verified, subject_batch, &limits)
            .expect("verify exact finalized Fidelity and Oracle MPT openings");
        fidelity_slot_count = fidelity_slot_count
            .checked_add(openings.fidelity.ordered_slots.len())
            .expect("fixture Fidelity slot count");
        oracle_slot_count = openings.oracle.ordered_slots.len();
        let materialized =
            materialize_authenticated_openings(&openings, &support::protocol_bundle(), &limits)
                .expect("materialize source-specific authenticated openings");
        fidelity_openings.push(materialized.fidelity);
        match &oracle_opening {
            None => oracle_opening = Some(materialized.oracle),
            Some(existing) => assert_eq!(
                existing, &materialized.oracle,
                "every bounded owner request must return the same canonical Oracle opening"
            ),
        }
    }
    let cas = FilesystemCas::open(
        Path::new(&datadir).join("ocomp-cas-v1"),
        CasWriterRole::SnapshotExporter,
        CasLimits {
            max_object_bytes: 1_048_576,
            max_total_bytes: 64 * 1_048_576,
        },
    )
    .expect("open production CAS writer boundary");
    let published = publish_input_artifact_set(
        &cas,
        &support::protocol_bundle(),
        InputArtifactContents {
            identity: InputArtifactIdentity {
                job_id: handoff.job_id,
                attempt: verified.intent.attempt,
                checkpoint: handoff.checkpoint.clone(),
                wwd: verified.intent.wwd,
                sealed_tribute_collection_key: verified.intent.sealed_tribute_collection_key,
                sealed_tribute_collection_root: verified.intent.sealed_tribute_collection_root,
            },
            canonical_tributes,
            fidelity_openings,
            oracle_opening: oracle_opening.expect("one canonical Oracle opening"),
        },
        &limits,
        poc_input_list_limits(),
    )
    .expect("publish and independently reconstruct complete input artifact closure");
    assert_eq!(published.tribute_count, summary.record_count);
    assert_eq!(published.tribute_nominal_total, summary.nominal_total);
    let committed = node
        .commit(CommitSnapshotExportV1 {
            job_id: handoff.job_id,
            pin_generation: handoff.pin_generation,
            lease_generation: handoff.lease_generation,
            manifest_hash: published.manifest_hash,
        })
        .expect("commit exact manifest hash to node-owned finalized pin");

    println!(
        "OCM_EXP_OPENED={}:{}:{}:{}:{}:{}:{}:{}:{}",
        hex::encode(acknowledgement.encode_fixed()),
        closure.collection_root,
        closure.exact_leaf_count,
        summary.nominal_total,
        ordered_ids.join(","),
        fidelity_slot_count,
        oracle_slot_count,
        published.manifest_hash,
        committed.record_hash,
    );
}

#[test]
fn exact_read_only_export_view_closes_root_count_and_each_commitment() {
    let directory = tempfile::tempdir().expect("temporary CE datadir");
    let day = WorldwideDay::new(20_260_724);
    let mongo = ManagedMongoReplicaSet::start("ocomp-finalized-export", false)
        .expect("start real Mongo replica set");
    let database = format!("outbe_ocomp_finalized_export_{}", std::process::id());
    let mongo_storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri: mongo.uri().to_owned(),
            database: database.clone(),
        })
        .expect("connect real Mongo storage"),
    );
    let bodies = [tribute(day, 0x11, 17), tribute(day, 0x22, 29)];
    let leaves = bodies
        .iter()
        .map(|body| {
            let payload = encode_tribute_v1(body).expect("canonical Tribute payload");
            let commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                BODY_SCHEMA_V1,
                body.tribute_id,
                &payload,
            )
            .expect("canonical Tribute commitment");
            (body.tribute_id, commitment)
        })
        .collect::<Vec<_>>();
    let mutations = leaves
        .iter()
        .map(|(tribute_id, commitment)| FinalLeafMutation {
            entity: EntityRef::Tribute(*tribute_id),
            final_leaf: Some(*commitment),
        })
        .collect::<Vec<_>>();
    let nominal_total = bodies
        .iter()
        .try_fold(U256::ZERO, |total, body| {
            total.checked_add(body.nominal_amount_minor)
        })
        .expect("fixture nominal total");
    let (ce_sealed_root, collection_root) = materialize_fixture_roots(day, &mutations);
    let intent = export_intent(
        day,
        ce_sealed_root,
        collection_root,
        u32::try_from(leaves.len()).expect("fixture count"),
        nominal_total,
    );
    let lysis_fixture = finalized_lysis_input_fixture(intent, &bodies, &poc_schema_limits());
    let opening_provider = lysis_fixture.opening_provider.clone();
    let expected_subjects = lysis_fixture.subjects.clone();
    let proof_fixture = lysis_fixture.finalized;
    let finalized_block = proof_fixture.block.clone();
    let block_hash = proof_fixture.header_hash;
    assert_eq!(finalized_block.block_hash(), block_hash);
    project_tribute_bodies(mongo_storage.clone(), block_hash, &bodies);
    let ahead_projection_hash = B256::repeat_byte(0x44);
    project_empty_block(mongo_storage.clone(), 2, ahead_projection_hash);

    let service = service(directory.path());
    let parent = service
        .open_parent(genesis_identity())
        .expect("open genesis parent");
    let provisional = parent
        .prepare_seal(1, &mutations, &[])
        .expect("prepare production CE candidate");
    assert_eq!(
        provisional.new_root(),
        ce_sealed_root,
        "proof intent must bind the independently pre-materialized CE root"
    );
    service
        .publish_candidate(block_hash, provisional)
        .expect("publish production CE candidate");
    let authoritative_root = service
        .candidate(1, block_hash)
        .expect("read CE candidate")
        .expect("candidate exists")
        .new_root();
    service
        .apply_finalized(1, block_hash, authoritative_root)
        .expect("finalize CE candidate");

    let marker = service.finalized_marker().expect("finalized CE marker");
    let exact = exact_identity(marker);
    let collection_root = service
        .open_parent(exact)
        .expect("open finalized parent")
        .partition_root_verified(PartitionRef::TributeWwd(day), exact.root)
        .expect("authenticate partition root")
        .expect("Tribute partition is present");
    assert_eq!(
        collection_root, proof_fixture.intent.sealed_tribute_collection_root,
        "proof intent must bind the exact finalized partition root"
    );
    let mut ordered_ids = bodies
        .iter()
        .map(|body| body.tribute_id)
        .collect::<Vec<_>>();
    ordered_ids.sort_unstable();
    let ordered_ids = ordered_ids
        .into_iter()
        .map(|tribute_id| hex::encode(tribute_id.as_bytes()))
        .collect::<Vec<_>>()
        .join(",");
    let candidate = CandidatePinV1 {
        block_number: finalized_block.number(),
        block_hash,
        state_root: proof_fixture.state_root,
        intent_id: proof_fixture.intent_id,
        wwd: day.value(),
        ce_sealed_root,
        protocol_bundle_hash: endpoint_identity().protocol_bundle_hash,
        deadline_height: proof_fixture.intent.deadline_height,
    };
    let job_id = proof_fixture.job_id;
    let projection_containment = RethProjectionContainmentAuthority::new(
        proof_fixture
            .canonical_history
            .clone()
            .with_canonical_block(2, ahead_projection_hash),
    );
    projection_containment
        .require_contains(
            ProjectionCheckpoint {
                block_number: 2,
                block_hash: ahead_projection_hash,
            },
            ProjectionCheckpoint {
                block_number: 1,
                block_hash,
            },
        )
        .expect("production Reth adapter accepts canonical ahead containment");
    let projection_containment = Arc::new(projection_containment);
    let source = Arc::new(LeaseFixtureSource {
        block_hash,
        finalized: FinalizedJobPinV1 { candidate, job_id },
        proof: proof_fixture.proof,
        opening_provider,
    });
    let retention = Arc::new(OcompRetentionCoordinator::open(
        directory.path().join("ocomp-retention"),
        source,
    ));
    retention
        .prepare_candidate(&finalized_block)
        .expect("pin exact finalized export candidate");
    retention
        .reconcile_finalized(&finalized_block)
        .expect("finalize exact export candidate");
    let uid = effective_uid().expect("effective OCOMP test uid");
    let node = Arc::new(
        OcompControlServer::new(retention, uid, endpoint_identity(), 1, poc_schema_limits())
            .expect("node OCOMP control")
            .with_snapshot_export(service.clone(), projection_containment, uid),
    );
    let supervisor_socket = directory.path().join("supervisor-node.sock");
    let exporter_socket = directory.path().join("exporter-node.sock");
    let _controls = RunningNodeControls::start(node, &supervisor_socket, &exporter_socket);
    let handoff = open_snapshot_from_supervisor(&supervisor_socket, uid);
    assert_eq!(handoff.job_id, job_id);

    let output = run_exporter_child(
        directory.path(),
        &exporter_socket,
        uid,
        false,
        mongo.uri(),
        &database,
    );
    assert!(
        output.status.success(),
        "exporter child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("child stdout is UTF-8");
    let opened = stdout
        .lines()
        .find_map(|line| line.split_once("OCM_EXP_OPENED=").map(|(_, opened)| opened))
        .unwrap_or_else(|| panic!("child emitted authenticated open result; stdout={stdout:?}"));
    let mut fields = opened.split(':');
    let _acknowledgement = hex::decode(fields.next().expect("lease ack")).expect("lease ack hex");
    assert_eq!(
        fields.next().expect("collection root"),
        collection_root.to_string()
    );
    assert_eq!(fields.next().expect("leaf count"), leaves.len().to_string());
    assert_eq!(
        fields.next().expect("nominal total"),
        nominal_total.to_string()
    );
    assert_eq!(fields.next().expect("ordered IDs"), ordered_ids);
    assert!(
        fields
            .next()
            .expect("Fidelity slot count")
            .parse::<usize>()
            .expect("Fidelity slot count is usize")
            > expected_subjects.owners.len(),
        "Fidelity opening includes authenticated count and detail slots"
    );
    assert!(
        fields
            .next()
            .expect("Oracle slot count")
            .parse::<usize>()
            .expect("Oracle slot count is usize")
            > expected_subjects.settlement_isos.len(),
        "Oracle opening includes authenticated count and detail slots"
    );
    let manifest_hash = fields
        .next()
        .expect("manifest hash")
        .parse::<B256>()
        .expect("manifest hash is B256");
    assert!(!manifest_hash.is_zero());
    let commit_record_hash = fields
        .next()
        .expect("snapshot export commit record hash")
        .parse::<B256>()
        .expect("snapshot export commit record hash is B256");
    assert!(!commit_record_hash.is_zero());
    assert!(fields.next().is_none());

    assert_eq!(
        service
            .export_lease_status(handoff.lease_generation)
            .expect("lease status"),
        ExportLeaseStatus::Opened
    );

    // A valid canonical Mongo body under the same identity is still only
    // transport: changing one nominal changes its CES1 commitment and the
    // exporter must abstain before emitting an authenticated result.
    let mut mutated = from_canonical_body(bodies[0].clone());
    mutated.nominal_amount_minor = mutated
        .nominal_amount_minor
        .checked_add(U256::from(1))
        .expect("fixture nominal mutation");
    TributeRepositoryWriter::new(mongo_storage.clone(), mongo_storage)
        .put(&mutated)
        .expect("mutate real Mongo transport through public repository");
    let mutation = run_exporter_child(
        directory.path(),
        &exporter_socket,
        uid,
        true,
        mongo.uri(),
        &database,
    );
    assert!(!mutation.status.success());
    assert!(
        !String::from_utf8_lossy(&mutation.stdout).contains("OCM_EXP_OPENED="),
        "mutated Mongo transport must not yield an authenticated export"
    );
}

fn run_exporter_child(
    ce_datadir: &Path,
    node_socket: &Path,
    node_uid: u32,
    skip_ack: bool,
    mongo_uri: &str,
    mongo_database: &str,
) -> Output {
    let mut command = Command::new(env::current_exe().expect("integration test executable"));
    command
        .arg("--exact")
        .arg("ocm_exp_001_child")
        .arg("--nocapture")
        .env(CHILD_MODE, "open-exact")
        .env(CHILD_DATADIR, ce_datadir)
        .env(CHILD_NODE_SOCKET, node_socket)
        .env(CHILD_NODE_UID, node_uid.to_string())
        .env(CHILD_MONGO_URI, mongo_uri)
        .env(CHILD_MONGO_DATABASE, mongo_database);
    if skip_ack {
        command.env(CHILD_SKIP_ACK, "1");
    }
    command.output().expect("spawn real exporter child process")
}

#[derive(Clone)]
struct LeaseFixtureSource {
    block_hash: B256,
    finalized: FinalizedJobPinV1,
    proof: FinalizedIntentProofV1,
    opening_provider: LysisOpeningProvider,
}

impl FinalizedInputProofSource for LeaseFixtureSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        Ok((block.block_hash() == self.block_hash).then_some(self.finalized.candidate))
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        if candidate != self.finalized.candidate {
            return Err(RetentionError::Source(
                "finalized export fixture candidate changed".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(self.finalized))
    }

    fn build_finalized_intent_proof(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        if candidate != self.finalized.candidate {
            return Err(RetentionError::Source(
                "finalized export fixture proof candidate changed".to_owned(),
            ));
        }
        Ok(self.proof.clone())
    }

    fn build_lysis_openings(
        &self,
        candidate: CandidatePinV1,
        subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, RetentionError> {
        if candidate != self.finalized.candidate {
            return Err(RetentionError::Source(
                "finalized export fixture opening candidate changed".to_owned(),
            ));
        }
        outbe_node::ocomp::build_lysis_openings(
            &self.opening_provider,
            &poc_schema_limits(),
            candidate,
            subjects,
        )
    }
}

struct RunningNodeControls {
    shutdown: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<std::io::Result<()>>>,
}

impl RunningNodeControls {
    fn start(
        server: Arc<OcompControlServer>,
        supervisor_socket: &Path,
        exporter_socket: &Path,
    ) -> Self {
        let supervisor =
            UnixListener::bind(supervisor_socket).expect("bind supervisor-node OCOMP UDS");
        let exporter = UnixListener::bind(exporter_socket).expect("bind exporter-node OCOMP UDS");
        let shutdown = Arc::new(AtomicBool::new(false));
        let supervisor_shutdown = Arc::clone(&shutdown);
        let supervisor_server = Arc::clone(&server);
        let supervisor_thread =
            thread::spawn(move || supervisor_server.serve_until(supervisor, &supervisor_shutdown));
        let exporter_shutdown = Arc::clone(&shutdown);
        let exporter_thread = thread::spawn(move || {
            server.serve_snapshot_exporter_until(exporter, &exporter_shutdown)
        });
        Self {
            shutdown,
            threads: vec![supervisor_thread, exporter_thread],
        }
    }
}

impl Drop for RunningNodeControls {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            thread
                .join()
                .expect("node control thread must not panic")
                .expect("node control listener must stop cleanly");
        }
    }
}

fn open_snapshot_from_supervisor(node_socket: &Path, node_uid: u32) -> SnapshotHandoffV1 {
    let limits = poc_schema_limits();
    let stream = UnixStream::connect(node_socket).expect("connect supervisor-node OCOMP UDS");
    let mut session = ControlClientSession::connect(
        stream,
        ClientPolicy::supervisor_to_node(node_uid, endpoint_identity(), limits),
    )
    .expect("open supervisor control session");
    session.handshake().expect("supervisor-node handshake");
    let list = ListFinalizedJobsV1 {
        after_cursor: 0,
        limit: MAX_FINALIZED_JOBS_PER_RESPONSE,
    };
    session
        .send_request(
            NodeMessageKind::ListFinalizedJobs as u16,
            list.encode_body(&limits).expect("encode finalized list"),
        )
        .expect("request finalized list");
    let response = session.receive_response().expect("receive finalized list");
    assert_eq!(response.message_kind, NodeMessageKind::Response as u16);
    let jobs = ListFinalizedJobsResponseV1::decode_body(&response.body, &limits)
        .expect("decode finalized list");
    let job = jobs.jobs.into_iter().next().expect("one finalized job");
    session
        .send_request(
            NodeMessageKind::OpenSnapshotLease as u16,
            OpenSnapshotLeaseV1 { job_id: job.job_id }
                .encode_body(&limits)
                .expect("encode snapshot open"),
        )
        .expect("request snapshot open");
    let response = session
        .receive_response()
        .expect("receive snapshot handoff");
    assert_eq!(response.message_kind, NodeMessageKind::Response as u16);
    SnapshotHandoffV1::decode_body(&response.body, &limits).expect("decode snapshot handoff")
}

fn project_tribute_bodies(storage: Arc<MongoStorage>, block_hash: B256, bodies: &[TributeBodyV1]) {
    let mut projection = OffchainDataProjection::open(
        ProjectionConfig {
            chain_id: environment().chain_id,
            genesis_hash: environment().genesis_hash,
            start_block: 1,
        },
        storage.clone(),
        storage,
    )
    .expect("open production off-chain projection");
    let logs = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| {
            let canonical_payload = encode_tribute_v1(body).expect("canonical projected body");
            let commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                BODY_SCHEMA_V1,
                body.tribute_id,
                &canonical_payload,
            )
            .expect("projected body commitment");
            FinalizedLog {
                log_index: u64::try_from(index).expect("fixture log index"),
                emitter: TRIBUTE_ADDRESS,
                data: ITribute::TributeBodyStored {
                    tributeId: Bytes::copy_from_slice(body.tribute_id.as_bytes()),
                    commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                    schemaVersion: BODY_SCHEMA_V1,
                    previousCommitment: B256::ZERO,
                    newCommitment: B256::from(*commitment.as_bytes()),
                    canonicalPayload: Bytes::from(canonical_payload),
                }
                .encode_log_data(),
            }
        })
        .collect();
    projection
        .project_block(&FinalizedBlock {
            number: 1,
            hash: block_hash,
            receipts: vec![FinalizedReceipt {
                tx_hash: B256::repeat_byte(0x43),
                transaction_index: 0,
                success: true,
                logs,
            }],
        })
        .expect("project finalized Tribute events into real Mongo");
}

fn project_empty_block(storage: Arc<MongoStorage>, number: u64, block_hash: B256) {
    let mut projection = OffchainDataProjection::open(
        ProjectionConfig {
            chain_id: environment().chain_id,
            genesis_hash: environment().genesis_hash,
            start_block: 1,
        },
        storage.clone(),
        storage,
    )
    .expect("reopen production off-chain projection");
    projection
        .project_block(&FinalizedBlock {
            number,
            hash: block_hash,
            receipts: Vec::new(),
        })
        .expect("advance Mongo projection beyond the finalized OCOMP job");
}

fn materialize_fixture_roots(day: WorldwideDay, mutations: &[FinalLeafMutation]) -> (B256, B256) {
    let directory = tempfile::tempdir().expect("temporary CE root materialization");
    let service = service(directory.path());
    let parent = service
        .open_parent(genesis_identity())
        .expect("open root-materialization parent");
    let provisional = parent
        .prepare_seal(1, mutations, &[])
        .expect("prepare root-materialization candidate");
    let ce_root = provisional.new_root();
    let block_hash = B256::repeat_byte(0xee);
    service
        .publish_candidate(block_hash, provisional)
        .expect("publish root-materialization candidate");
    service
        .apply_finalized(1, block_hash, ce_root)
        .expect("finalize root-materialization candidate");
    let marker = service
        .finalized_marker()
        .expect("root-materialization marker");
    let collection_root = service
        .open_parent(exact_identity(marker))
        .expect("open root-materialization snapshot")
        .partition_root_verified(PartitionRef::TributeWwd(day), ce_root)
        .expect("authenticate root-materialization partition")
        .expect("fixture Tribute partition is present");
    (ce_root, collection_root)
}

fn export_intent(
    day: WorldwideDay,
    ce_sealed_root: B256,
    collection_root: B256,
    exact_count: u32,
    exact_nominal_total: U256,
) -> JobIntentV1 {
    let (_, collection_key) = partition_collection_key(PartitionRef::TributeWwd(day))
        .expect("canonical fixture collection key");
    let collection_key = B256::from(*collection_key.as_bytes());
    JobIntentV1 {
        chain_id: environment().chain_id,
        genesis_hash: environment().genesis_hash,
        fork_id: FORK_ID,
        wwd: day.value(),
        pending_nonce: 1,
        attempt: 1,
        protocol_bundle_hash: endpoint_identity().protocol_bundle_hash,
        ce_sealed_root,
        sealed_tribute_collection_key: collection_key,
        sealed_tribute_collection_root: collection_root,
        authenticated_day_count: exact_count,
        authenticated_day_nominal: exact_nominal_total,
        pre_admission_envelope_hash: B256::repeat_byte(0x15),
        source_availability_policy_id: B256::repeat_byte(0x16),
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
            request_budget_split_receipt_hash: B256::repeat_byte(0x17),
        },
        logical_evaluation_height: 1,
        logical_evaluation_time: 1_000,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: day.value(),
                source_generation: 1,
                collection_key,
                sealed_collection_root: collection_root,
                exact_count,
                exact_nominal_total,
            },
            nod: NodTargetPreconditionV1 {
                wwd: day.value(),
                target_generation: 1,
                namespace_root_before: B256::repeat_byte(0x18),
                max_nod_count: exact_count,
            },
            contributors: ContributorTargetPreconditionV1 {
                series_id: day.value(),
                expected_series_version: 1,
                max_contributor_count: exact_count,
                max_eligible_nominal_total: exact_nominal_total,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: day.value(),
                pending_nonce: 1,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_committee_snapshot_hash: B256::repeat_byte(0x19),
        custody_committee_epoch_hash: None,
        deadline_height: 110,
    }
}

fn tribute(day: WorldwideDay, suffix: u8, nominal: u64) -> TributeBodyV1 {
    let owner = Address::repeat_byte(suffix);
    TributeBodyV1 {
        tribute_id: derive_poseidon_entity_id(owner, day).expect("canonical Tribute identity"),
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(nominal - 1),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(nominal),
        reference_currency: 978,
        tribute_price_minor: U256::from(3),
        exclude_from_intex_issuance: false,
    }
}

fn environment() -> EnvironmentIdentity {
    EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 10,
        genesis_hash: B256::repeat_byte(0x10),
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: outbe_compressed_entities::CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
        vendor_revision: VENDOR_REVISION.to_owned(),
    }
}

fn genesis() -> FinalizedMarker {
    FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: environment().genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).expect("canonical empty CE root"),
    }
}

fn genesis_identity() -> ExactParentIdentity {
    exact_identity(genesis())
}

fn exact_identity(marker: FinalizedMarker) -> ExactParentIdentity {
    ExactParentIdentity {
        commitment_scheme_version: marker.commitment_scheme_version,
        block_number: marker.height,
        block_hash: marker.block_hash,
        root: marker.new_root,
    }
}

fn endpoint_identity() -> EndpointIdentity {
    let limits = poc_schema_limits();
    EndpointIdentity {
        chain_id: environment().chain_id,
        genesis_hash: environment().genesis_hash,
        boot_nonce: B256::repeat_byte(0x12),
        protocol_bundle_hash: support::protocol_bundle()
            .protocol_bundle_hash(&limits)
            .expect("fixture protocol bundle hash"),
    }
}

fn service(datadir: &std::path::Path) -> Arc<CompressedTreeService> {
    let db = CeMdbx::open(datadir, environment(), genesis()).expect("open production CE MDBX");
    Arc::new(
        CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 2,
                max_encoded_bytes: 1_000_000,
            },
        )
        .expect("open production CE service"),
    )
}
