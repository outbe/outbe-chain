mod support;

use std::env;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_fidelity::fidelity_opening_slot_plan_v1;
use outbe_lysis::program_v1::artifacts::{
    decode_amount_run, decode_finalized_output_run, decode_fixed_reduce_output,
    decode_gratis_prefix_down_output, decode_gratis_segment_summary,
    GratisPrefixDownOutputV1,
};
use outbe_lysis::program_v1::planner::{
    LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1,
};
use outbe_oracle::oracle_opening_slot_plan_v1;
use outbe_ocomp::bundle::PinnedProtocolBundle;
use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
use outbe_ocomp::control::{
    effective_uid, effective_user_name, poc_schema_limits, ClientPolicy, ControlClientSession,
    EndpointIdentity,
};
use outbe_ocomp::inbox::{WorkerInbox, WorkerInboxLimits};
use outbe_ocomp::input_artifacts::{
    derive_input_chunk_ref, poc_input_list_limits, publish_input_artifact_set,
    InputArtifactContents, InputArtifactIdentity,
};
use outbe_ocomp::worker::{run_one_from_inherited_fd, WorkerConfig};
use outbe_ocomp_protocol::common::{BoundedBytes, ProofBytes};
use outbe_ocomp_protocol::input::{
    materialize_authenticated_openings, CheckpointIdentityV1, InputChunkKind, InputManifestV1,
};
use outbe_ocomp_protocol::opening::{
    LysisOpeningsProofV1, OpeningSubjectsV1, RawContractOpeningProofV1, RawStorageSlotV1,
};
use outbe_ocomp_protocol::unit::{
    CanonicalInputRefV1, EntityIdHalfOpenRange, InputPurpose, InputSourceKind, PlanCommitmentV1,
    UnitArtifactV1, UnitInterval, UnitPhase, UnitSpecV1,
};
use outbe_ocomp_protocol::{
    ordered_list_root, CasObjectRefV1, ListKind, ObjectKind, OrderedListLimits, RunUnitV1,
    SchemaLimits, UnitFinishedStatus, UnitFinishedV1, WorkerMessageKind,
};
use tempfile::tempdir;

struct RunningWorker {
    child: Child,
    client: ControlClientSession,
    unit_id: B256,
}

const CHILD_MODE: &str = "OUTBE_OCOMP_TEST_WORKER_CHILD";
const CHILD_USER: &str = "OUTBE_OCOMP_TEST_WORKER_USER";
const CHILD_CHAIN_ID: &str = "OUTBE_OCOMP_TEST_CHAIN_ID";
const CHILD_GENESIS: &str = "OUTBE_OCOMP_TEST_GENESIS";
const CHILD_BOOT_NONCE: &str = "OUTBE_OCOMP_TEST_BOOT_NONCE";
const CHILD_BUNDLE: &str = "OUTBE_OCOMP_TEST_BUNDLE";
const CHILD_GENERATION: &str = "OUTBE_OCOMP_TEST_GENERATION";
const CHILD_CAS_ROOT: &str = "OUTBE_OCOMP_TEST_CAS_ROOT";
const CHILD_CAS_OBJECT_CAP: &str = "OUTBE_OCOMP_TEST_CAS_OBJECT_CAP";
const CHILD_CAS_TOTAL_CAP: &str = "OUTBE_OCOMP_TEST_CAS_TOTAL_CAP";
const CHILD_INBOX_ROOT: &str = "OUTBE_OCOMP_TEST_INBOX_ROOT";

fn identity(boot: u8) -> EndpointIdentity {
    let limits = poc_schema_limits();
    EndpointIdentity {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        boot_nonce: B256::repeat_byte(boot),
        protocol_bundle_hash: support::protocol_bundle()
            .protocol_bundle_hash(&limits)
            .expect("fixture protocol bundle hash"),
    }
}

#[test]
fn real_worker_processes_execute_through_output_finalize() {
    if env::var_os(CHILD_MODE).is_some() {
        run_child_worker();
        return;
    }

    let directory = tempdir().expect("worker fixture");
    let cas_limits = CasLimits {
        max_object_bytes: 1024 * 1024,
        max_total_bytes: 8 * 1024 * 1024,
    };
    let cas = FilesystemCas::open(directory.path(), CasWriterRole::Supervisor, cas_limits)
        .expect("open CAS");
    let inbox_root = directory.path().join("worker-inbox");
    let inbox_limits = WorkerInboxLimits {
        max_artifact_bytes: 1024 * 1024,
        max_total_bytes: 4 * 1024 * 1024,
    };
    let limits = poc_schema_limits();
    let bundle = support::protocol_bundle();
    let job_id = B256::repeat_byte(0x31);
    let day = WorldwideDay::new(20_260_724);
    let owner = Address::repeat_byte(0x51);
    let finalized_state_root = B256::repeat_byte(0x52);
    let tribute = TributeBodyV1 {
        tribute_id: derive_poseidon_entity_id(owner, day).expect("fixture Tribute id"),
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(9),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(10),
        reference_currency: 978,
        tribute_price_minor: U256::from(2),
        exclude_from_intex_issuance: false,
    };
    let fidelity_raw = RawContractOpeningProofV1 {
        contract_address: Address::repeat_byte(0x54),
        state_root: finalized_state_root,
        ordered_slots: fidelity_opening_slot_plan_v1(owner, 0, 0)
            .expect("fixture Fidelity slot plan")
            .slots
            .into_iter()
            .map(|slot| RawStorageSlotV1 {
                slot,
                value: U256::ZERO,
            })
            .collect(),
        account_proof: ProofBytes(vec![0xa1]),
        storage_proof: ProofBytes(vec![0xb1]),
    };
    let usd_pair = keccak256(b"USD/COEN");
    let eur_pair = keccak256(b"EUR/COEN");
    let oracle_plan = oracle_opening_slot_plan_v1(
        day,
        &[(840, usd_pair), (978, eur_pair)],
        2,
        0,
        0,
    )
    .expect("fixture Oracle slot plan");
    let scale = U256::from(1_000_000_000_000_000_000_u64);
    let oracle_values = [
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
    assert_eq!(oracle_plan.slots.len(), oracle_values.len());
    let oracle_raw = RawContractOpeningProofV1 {
        contract_address: Address::repeat_byte(0x56),
        state_root: finalized_state_root,
        ordered_slots: oracle_plan
            .slots
            .into_iter()
            .zip(oracle_values)
            .map(|(slot, value)| RawStorageSlotV1 { slot, value })
            .collect(),
        account_proof: ProofBytes(vec![0xa1]),
        storage_proof: ProofBytes(vec![0xb1]),
    };
    let materialized = materialize_authenticated_openings(
        &LysisOpeningsProofV1 {
            protocol_bundle_hash: bundle
                .protocol_bundle_hash(&limits)
                .expect("fixture bundle hash"),
            job_id,
            finalized_block_hash: B256::repeat_byte(0x53),
            finalized_state_root,
            wwd: day.value(),
            subjects: OpeningSubjectsV1 {
                owners: vec![owner],
                settlement_isos: vec![840, 978],
            },
            fidelity: fidelity_raw,
            oracle: oracle_raw,
        },
        &bundle,
        &limits,
    )
    .expect("materialize fixture openings");
    let published = publish_input_artifact_set(
        &cas,
        &bundle,
        InputArtifactContents {
            identity: InputArtifactIdentity {
                job_id,
                attempt: 1,
                checkpoint: CheckpointIdentityV1 {
                    finalized_block_number: 90,
                    finalized_block_hash: B256::repeat_byte(0x53),
                    finalized_state_root,
                    finalized_ce_root: B256::repeat_byte(0x58),
                    ce_schema_version: 1,
                },
                wwd: day.value(),
                sealed_tribute_collection_key: B256::repeat_byte(0x59),
                sealed_tribute_collection_root: B256::repeat_byte(0x5a),
            },
            canonical_tributes: vec![
                encode_tribute_v1(&tribute).expect("canonical fixture Tribute")
            ],
            fidelity_openings: vec![materialized.fidelity],
            oracle_opening: materialized.oracle,
        },
        &limits,
        poc_input_list_limits(),
    )
    .expect("publish worker fixture inputs");
    let tribute_ref = published
        .ordered_chunk_refs
        .iter()
        .find(|reference| {
            cas.read_verified(reference)
                .ok()
                .and_then(|object| derive_input_chunk_ref(&object, &bundle, &limits).ok())
                .is_some_and(|derived| derived.reference.kind == InputChunkKind::Tribute)
        })
        .cloned()
        .expect("fixture Tribute input reference");
    let tribute_object = cas
        .read_verified(&tribute_ref)
        .expect("read fixture Tribute chunk");
    let derived_tribute = derive_input_chunk_ref(&tribute_object, &bundle, &limits)
        .expect("derive fixture Tribute reference")
        .reference;
    let canonical_inputs = vec![
        CanonicalInputRefV1 {
            purpose: InputPurpose::InputManifest,
            source_kind: InputSourceKind::AuthenticatedRoot,
            source_id: published.manifest_hash,
            record_count_limit: 1,
            max_encoded_bytes: published.manifest_ref.encoded_bytes,
            max_decoded_bytes: published.manifest_ref.encoded_bytes,
        },
        CanonicalInputRefV1 {
            purpose: InputPurpose::TributeStream,
            source_kind: InputSourceKind::AuthenticatedRoot,
            source_id: derived_tribute.semantic_digest,
            record_count_limit: derived_tribute.record_count,
            max_encoded_bytes: derived_tribute.encoded_bytes,
            max_decoded_bytes: derived_tribute.encoded_bytes,
        },
    ];
    let protocol_bundle_hash = bundle
        .protocol_bundle_hash(&limits)
        .expect("fixture bundle hash");
    let interval_start = outbe_ocomp_protocol::common::EntityId36(*tribute.tribute_id.as_bytes());
    let spec = UnitSpecV1 {
        protocol_bundle_hash,
        job_id,
        attempt: 1,
        phase: UnitPhase::Enumerate,
        interval: UnitInterval::EntityIdRange(EntityIdHalfOpenRange {
            start: interval_start,
            end: None,
        }),
        canonical_ordered_inputs: canonical_inputs,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    let canonical_spec = spec.encode_canonical(&limits).expect("canonical unit spec");
    let primary_work_unit_root = ordered_list_root(
        ListKind::UnitSpecificationsArtifacts,
        std::slice::from_ref(&canonical_spec),
        OrderedListLimits::new(1, limits.codec.max_body_bytes, 32),
    )
    .expect("primary unit root");
    let plan = PlanCommitmentV1 {
        protocol_bundle_hash,
        job_id,
        attempt: 1,
        input_manifest_hash: published.manifest_hash,
        wwd: day.value(),
        lysis_budget: U256::from(99_000_000_u64),
        logical_evaluation_time: 1_784_765_900,
        tribute_count: published.tribute_count,
        max_tributes_per_work_shard: 256,
        primary_work_unit_count: 1,
        primary_work_unit_root,
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    let plan_hash = plan.plan_hash(&limits).expect("fixture plan hash");
    let plan_ref = cas
        .publish_bytes(
            &plan
                .encode_canonical_record(&limits)
                .expect("canonical plan commitment"),
        )
        .expect("plan object");
    let user = effective_user_name().expect("effective user");
    let uid = effective_uid().expect("effective uid");

    let mut workers = Vec::new();
    for index in 0_u8..4 {
        let (parent_stream, child_stream) = UnixStream::pair().expect("worker socket pair");
        let child_fd: OwnedFd = child_stream.into();
        let worker_identity = identity(0xA0 + index);
        let mut command = Command::new(env::current_exe().expect("current Rust test binary"));
        command
            .args([
                "--exact",
                "real_worker_processes_execute_through_output_finalize",
                "--nocapture",
            ])
            .env(CHILD_MODE, "1")
            .env(CHILD_USER, &user)
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
            .env(CHILD_GENERATION, (100_u64 + u64::from(index)).to_string())
            .env(CHILD_CAS_ROOT, directory.path())
            .env(
                CHILD_CAS_OBJECT_CAP,
                cas_limits.max_object_bytes.to_string(),
            )
            .env(CHILD_CAS_TOTAL_CAP, cas_limits.max_total_bytes.to_string())
            .env(CHILD_INBOX_ROOT, &inbox_root)
            .stdin(Stdio::from(child_fd))
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = command.spawn().expect("spawn production worker");

        let client_identity = EndpointIdentity {
            boot_nonce: B256::repeat_byte(0xB0 + index),
            ..worker_identity
        };
        let mut client = ControlClientSession::connect(
            parent_stream,
            ClientPolicy::supervisor_to_worker(uid, client_identity, limits),
        )
        .expect("worker client");
        client.handshake().expect("worker handshake");

        let unit_id = spec.unit_id(&limits).expect("unit id");
        let request = RunUnitV1 {
            protocol_bundle_hash: spec.protocol_bundle_hash,
            job_id: spec.job_id,
            attempt: spec.attempt,
            plan_hash,
            unit_index: 0,
            canonical_unit_spec: BoundedBytes(canonical_spec.clone()),
            unit_membership_siblings: Vec::new(),
            plan_ref: plan_ref.clone(),
            input_manifest_ref: published.manifest_ref.clone(),
            ordered_input_refs: vec![tribute_ref.clone()],
        };
        client
            .send_request(
                WorkerMessageKind::RunUnit as u16,
                request.encode_body(&limits).expect("run unit body"),
            )
            .expect("send exact unit");
        workers.push(RunningWorker {
            child,
            client,
            unit_id,
        });
    }

    let mut finished_reports = Vec::new();
    for mut worker in workers {
        let frame = worker.client.receive_response().expect("worker response");
        assert_eq!(frame.message_kind, WorkerMessageKind::UnitFinished as u16);
        let finished = UnitFinishedV1::decode_body(&frame.body, &limits).expect("finished body");
        assert_eq!(finished.unit_id, worker.unit_id);
        assert_eq!(finished.status, UnitFinishedStatus::Success);
        assert!(finished.exact_staged_bytes > 0);
        assert_ne!(finished.transport_digest, B256::ZERO);
        finished_reports.push(finished);
        let output = worker.child.wait_with_output().expect("worker exit");
        assert!(
            output.status.success(),
            "worker failed: {} (expected peer uid {uid})",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(finished_reports.windows(2).all(|pair| pair[0] == pair[1]));
    let inbox = WorkerInbox::open(&inbox_root, inbox_limits).expect("open worker inbox");
    assert_eq!(inbox.artifact_count().unwrap(), 1);
    let verified = inbox
        .read_reported(
            finished_reports[0].unit_id,
            finished_reports[0].exact_staged_bytes,
            finished_reports[0].transport_digest,
        )
        .expect("read staged unit artifact");
    let artifact = UnitArtifactV1::decode_canonical(verified.bytes(), &limits)
        .expect("decode staged unit artifact");
    artifact.validate_against(&spec, &limits).unwrap();

    let mut producer_ref = cas
        .publish_bytes(verified.bytes())
        .expect("publish Enumerate producer artifact");
    producer_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let fidelity_ref = published
        .ordered_chunk_refs
        .iter()
        .find(|reference| {
            cas.read_verified(reference)
                .ok()
                .and_then(|object| derive_input_chunk_ref(&object, &bundle, &limits).ok())
                .is_some_and(|derived| derived.reference.kind == InputChunkKind::Fidelity)
        })
        .cloned()
        .expect("fixture Fidelity input reference");
    let manifest = InputManifestV1::decode_canonical(
        cas.read_verified(&published.manifest_ref)
            .expect("read fixture manifest")
            .bytes(),
        &limits,
    )
    .expect("decode fixture manifest");
    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash,
        job_id,
        attempt: 1,
        input_manifest_hash: published.manifest_hash,
        input_manifest_encoded_bytes: published.manifest_ref.encoded_bytes,
        fidelity_opening_root: manifest.fidelity_opening_root,
        oracle_opening_root: manifest.oracle_opening_root,
        wwd: day.value(),
        lysis_budget: plan.lysis_budget,
        logical_evaluation_time: plan.logical_evaluation_time,
        tribute_count: published.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: 1,
        reducer_spec_version: 1,
    })
    .expect("fixture planner");
    let fidelity_spec = planner
        .fidelity_map_unit_at(0, artifact.unit_id, &limits)
        .expect("derive Fidelity unit");
    let fidelity_unit_id = fidelity_spec.unit_id(&limits).expect("Fidelity UnitId");
    let (parent_stream, child_stream) = UnixStream::pair().expect("Fidelity worker socket pair");
    let child_fd: OwnedFd = child_stream.into();
    let worker_identity = identity(0xD0);
    let mut command = Command::new(env::current_exe().expect("current Rust test binary"));
    command
        .args([
            "--exact",
            "real_worker_processes_execute_through_output_finalize",
            "--nocapture",
        ])
        .env(CHILD_MODE, "1")
        .env(CHILD_USER, &user)
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
        .env(CHILD_GENERATION, "200")
        .env(CHILD_CAS_ROOT, directory.path())
        .env(
            CHILD_CAS_OBJECT_CAP,
            cas_limits.max_object_bytes.to_string(),
        )
        .env(CHILD_CAS_TOTAL_CAP, cas_limits.max_total_bytes.to_string())
        .env(CHILD_INBOX_ROOT, &inbox_root)
        .stdin(Stdio::from(child_fd))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn Fidelity worker");
    drop(command);
    let client_identity = EndpointIdentity {
        boot_nonce: B256::repeat_byte(0xD1),
        ..worker_identity
    };
    let mut client = ControlClientSession::connect(
        parent_stream,
        ClientPolicy::supervisor_to_worker(uid, client_identity, limits),
    )
    .expect("Fidelity worker client");
    client.handshake().expect("Fidelity worker handshake");
    client
        .send_request(
            WorkerMessageKind::RunUnit as u16,
            RunUnitV1 {
                protocol_bundle_hash,
                job_id,
                attempt: 1,
                plan_hash,
                unit_index: plan.primary_work_unit_count,
                canonical_unit_spec: BoundedBytes(
                    fidelity_spec
                        .encode_canonical(&limits)
                        .expect("canonical Fidelity spec"),
                ),
                unit_membership_siblings: Vec::new(),
                plan_ref: plan_ref.clone(),
                input_manifest_ref: published.manifest_ref.clone(),
                ordered_input_refs: vec![
                    producer_ref.clone(),
                    tribute_ref.clone(),
                    fidelity_ref,
                ],
            }
            .encode_body(&limits)
            .expect("Fidelity RunUnit body"),
        )
        .expect("send Fidelity unit");
    let output = child.wait_with_output().expect("Fidelity worker exit");
    assert!(
        output.status.success(),
        "Fidelity worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frame = client.receive_response().expect("Fidelity worker response");
    let finished =
        UnitFinishedV1::decode_body(&frame.body, &limits).expect("Fidelity finished body");
    assert_eq!(finished.status, UnitFinishedStatus::Success);
    assert_eq!(finished.unit_id, fidelity_unit_id);
    let fidelity_artifact = UnitArtifactV1::decode_canonical(
        inbox
            .read_reported(
                finished.unit_id,
                finished.exact_staged_bytes,
                finished.transport_digest,
            )
            .expect("read staged Fidelity artifact")
            .bytes(),
        &limits,
    )
    .expect("decode Fidelity artifact");
    fidelity_artifact
        .validate_against(&fidelity_spec, &limits)
        .expect("validate Fidelity artifact");

    let mut fidelity_producer_ref = cas
        .publish_bytes(
            &fidelity_artifact
                .encode_canonical(&limits)
                .expect("canonical Fidelity artifact"),
        )
        .expect("publish Fidelity producer artifact");
    fidelity_producer_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let reduce_spec = planner
        .fixed_reduce_unit_at(0, [Some(fidelity_artifact.unit_id), None], &limits)
        .expect("derive FixedReduce unit");
    let reduce_unit_id = reduce_spec.unit_id(&limits).expect("FixedReduce UnitId");
    let (parent_stream, child_stream) =
        UnixStream::pair().expect("FixedReduce worker socket pair");
    let child_fd: OwnedFd = child_stream.into();
    let worker_identity = identity(0xE0);
    let mut command = Command::new(env::current_exe().expect("current Rust test binary"));
    command
        .args([
            "--exact",
            "real_worker_processes_execute_through_output_finalize",
            "--nocapture",
        ])
        .env(CHILD_MODE, "1")
        .env(CHILD_USER, &user)
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
        .env(CHILD_GENERATION, "300")
        .env(CHILD_CAS_ROOT, directory.path())
        .env(
            CHILD_CAS_OBJECT_CAP,
            cas_limits.max_object_bytes.to_string(),
        )
        .env(CHILD_CAS_TOTAL_CAP, cas_limits.max_total_bytes.to_string())
        .env(CHILD_INBOX_ROOT, &inbox_root)
        .stdin(Stdio::from(child_fd))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn FixedReduce worker");
    drop(command);
    let client_identity = EndpointIdentity {
        boot_nonce: B256::repeat_byte(0xE1),
        ..worker_identity
    };
    let mut client = ControlClientSession::connect(
        parent_stream,
        ClientPolicy::supervisor_to_worker(uid, client_identity, limits),
    )
    .expect("FixedReduce worker client");
    client.handshake().expect("FixedReduce worker handshake");
    client
        .send_request(
            WorkerMessageKind::RunUnit as u16,
            RunUnitV1 {
                protocol_bundle_hash,
                job_id,
                attempt: 1,
                plan_hash,
                unit_index: plan.primary_work_unit_count * 2,
                canonical_unit_spec: BoundedBytes(
                    reduce_spec
                        .encode_canonical(&limits)
                        .expect("canonical FixedReduce spec"),
                ),
                unit_membership_siblings: Vec::new(),
                plan_ref: plan_ref.clone(),
                input_manifest_ref: published.manifest_ref.clone(),
                ordered_input_refs: vec![fidelity_producer_ref.clone()],
            }
            .encode_body(&limits)
            .expect("FixedReduce RunUnit body"),
        )
        .expect("send FixedReduce unit");
    let output = child.wait_with_output().expect("FixedReduce worker exit");
    assert!(
        output.status.success(),
        "FixedReduce worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frame = client
        .receive_response()
        .expect("FixedReduce worker response");
    let finished =
        UnitFinishedV1::decode_body(&frame.body, &limits).expect("FixedReduce finished body");
    assert_eq!(finished.status, UnitFinishedStatus::Success);
    assert_eq!(finished.unit_id, reduce_unit_id);
    let reduce_artifact = UnitArtifactV1::decode_canonical(
        inbox
            .read_reported(
                finished.unit_id,
                finished.exact_staged_bytes,
                finished.transport_digest,
            )
            .expect("read staged FixedReduce artifact")
            .bytes(),
        &limits,
    )
    .expect("decode FixedReduce artifact");
    reduce_artifact
        .validate_against(&reduce_spec, &limits)
        .expect("validate FixedReduce artifact");
    let reduced =
        decode_fixed_reduce_output(reduce_artifact.phase_payload(&limits).unwrap(), &limits)
            .expect("decode FixedReduce phase output");
    assert_eq!(reduced.aggregate.unwrap().tribute_count, 1);
    assert!(!reduced.ordered_fractions.is_empty());

    let mut reduce_producer_ref = cas
        .publish_bytes(
            &reduce_artifact
                .encode_canonical(&limits)
                .expect("canonical FixedReduce artifact"),
        )
        .expect("publish FixedReduce producer artifact");
    reduce_producer_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let amount_spec = planner
        .amount_map_unit_at(
            0,
            &spec,
            fidelity_artifact.unit_id,
            reduce_artifact.unit_id,
            &limits,
        )
        .expect("derive AmountMap unit");
    let amount_unit_id = amount_spec.unit_id(&limits).expect("AmountMap UnitId");
    let oracle_ref = published
        .ordered_chunk_refs
        .iter()
        .find(|reference| {
            cas.read_verified(reference)
                .ok()
                .and_then(|object| derive_input_chunk_ref(&object, &bundle, &limits).ok())
                .is_some_and(|derived| derived.reference.kind == InputChunkKind::Oracle)
        })
        .cloned()
        .expect("fixture Oracle input reference");
    let (parent_stream, child_stream) =
        UnixStream::pair().expect("AmountMap worker socket pair");
    let child_fd: OwnedFd = child_stream.into();
    let worker_identity = identity(0xF0);
    let mut command = Command::new(env::current_exe().expect("current Rust test binary"));
    command
        .args([
            "--exact",
            "real_worker_processes_execute_through_output_finalize",
            "--nocapture",
        ])
        .env(CHILD_MODE, "1")
        .env(CHILD_USER, &user)
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
        .env(CHILD_GENERATION, "400")
        .env(CHILD_CAS_ROOT, directory.path())
        .env(
            CHILD_CAS_OBJECT_CAP,
            cas_limits.max_object_bytes.to_string(),
        )
        .env(CHILD_CAS_TOTAL_CAP, cas_limits.max_total_bytes.to_string())
        .env(CHILD_INBOX_ROOT, &inbox_root)
        .stdin(Stdio::from(child_fd))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn AmountMap worker");
    drop(command);
    let client_identity = EndpointIdentity {
        boot_nonce: B256::repeat_byte(0xF1),
        ..worker_identity
    };
    let mut client = ControlClientSession::connect(
        parent_stream,
        ClientPolicy::supervisor_to_worker(uid, client_identity, limits),
    )
    .expect("AmountMap worker client");
    client.handshake().expect("AmountMap worker handshake");
    client
        .send_request(
            WorkerMessageKind::RunUnit as u16,
            RunUnitV1 {
                protocol_bundle_hash,
                job_id,
                attempt: 1,
                plan_hash,
                unit_index: 3,
                canonical_unit_spec: BoundedBytes(
                    amount_spec
                        .encode_canonical(&limits)
                        .expect("canonical AmountMap spec"),
                ),
                unit_membership_siblings: Vec::new(),
                plan_ref: plan_ref.clone(),
                input_manifest_ref: published.manifest_ref.clone(),
                ordered_input_refs: vec![
                    producer_ref.clone(),
                    fidelity_producer_ref.clone(),
                    reduce_producer_ref.clone(),
                    tribute_ref.clone(),
                    oracle_ref.clone(),
                ],
            }
            .encode_body(&limits)
            .expect("AmountMap RunUnit body"),
        )
        .expect("send AmountMap unit");
    let output = child.wait_with_output().expect("AmountMap worker exit");
    assert!(
        output.status.success(),
        "AmountMap worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frame = client.receive_response().expect("AmountMap worker response");
    let finished =
        UnitFinishedV1::decode_body(&frame.body, &limits).expect("AmountMap finished body");
    assert_eq!(finished.status, UnitFinishedStatus::Success);
    assert_eq!(finished.unit_id, amount_unit_id);
    let amount_artifact = UnitArtifactV1::decode_canonical(
        inbox
            .read_reported(
                finished.unit_id,
                finished.exact_staged_bytes,
                finished.transport_digest,
            )
            .expect("read staged AmountMap artifact")
            .bytes(),
        &limits,
    )
    .expect("decode AmountMap artifact");
    amount_artifact
        .validate_against(&amount_spec, &limits)
        .expect("validate AmountMap artifact");
    let amount = decode_amount_run(amount_artifact.phase_payload(&limits).unwrap(), &limits)
        .expect("decode AmountMap phase output");
    assert_eq!(amount.ordered_records.len(), 1);
    assert_eq!(amount.ordered_records[0].entry_price_minor, scale * U256::from(2));

    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)
        .expect("fixture plan topology");
    let prefix_offset = plan
        .primary_work_unit_count
        .checked_mul(3)
        .and_then(|offset| {
            offset.checked_add(topology.phase_unit_count(UnitPhase::FixedReduce))
        })
        .expect("GratisPrefix offset");
    let prefix_down_offset = prefix_offset
        .checked_add(topology.phase_unit_count(UnitPhase::GratisPrefix))
        .expect("GratisPrefixDown offset");

    let mut amount_producer_ref = cas
        .publish_bytes(
            &amount_artifact
                .encode_canonical(&limits)
                .expect("canonical AmountMap artifact"),
        )
        .expect("publish AmountMap producer artifact");
    amount_producer_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let prefix_leaf_spec = planner
        .gratis_prefix_unit_at(0, &[Some(amount_artifact.unit_id)], &limits)
        .expect("derive GratisPrefix leaf");
    let prefix_leaf_artifact = execute_real_worker_unit(
        500,
        0x91,
        &user,
        uid,
        directory.path(),
        cas_limits,
        &inbox_root,
        inbox_limits,
        limits,
        &prefix_leaf_spec,
        prefix_offset,
        plan_hash,
        plan_ref.clone(),
        published.manifest_ref.clone(),
        vec![amount_producer_ref],
    );
    prefix_leaf_artifact
        .validate_against(&prefix_leaf_spec, &limits)
        .expect("validate GratisPrefix leaf artifact");
    let leaf_summary = decode_gratis_segment_summary(
        prefix_leaf_artifact.phase_payload(&limits).unwrap(),
        &limits,
    )
    .expect("decode GratisPrefix leaf");
    assert_eq!(
        leaf_summary.checked_segment_gratis_total,
        amount.checked_segment_gratis_total
    );

    let mut prefix_leaf_ref = cas
        .publish_bytes(
            &prefix_leaf_artifact
                .encode_canonical(&limits)
                .expect("canonical GratisPrefix leaf artifact"),
        )
        .expect("publish GratisPrefix leaf artifact");
    prefix_leaf_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let prefix_root_spec = planner
        .gratis_prefix_unit_at(1, &[Some(prefix_leaf_artifact.unit_id), None], &limits)
        .expect("derive GratisPrefix root");
    let prefix_root_artifact = execute_real_worker_unit(
        501,
        0x93,
        &user,
        uid,
        directory.path(),
        cas_limits,
        &inbox_root,
        inbox_limits,
        limits,
        &prefix_root_spec,
        prefix_offset + 1,
        plan_hash,
        plan_ref.clone(),
        published.manifest_ref.clone(),
        vec![prefix_leaf_ref.clone()],
    );
    prefix_root_artifact
        .validate_against(&prefix_root_spec, &limits)
        .expect("validate GratisPrefix root artifact");
    assert_eq!(
        decode_gratis_segment_summary(
            prefix_root_artifact.phase_payload(&limits).unwrap(),
            &limits,
        )
        .expect("decode GratisPrefix root"),
        leaf_summary
    );

    let prefix_down_root_spec = planner
        .gratis_prefix_down_unit_at(0, &[Some(prefix_leaf_artifact.unit_id), None], &limits)
        .expect("derive GratisPrefixDown root");
    let prefix_down_root_artifact = execute_real_worker_unit(
        502,
        0x95,
        &user,
        uid,
        directory.path(),
        cas_limits,
        &inbox_root,
        inbox_limits,
        limits,
        &prefix_down_root_spec,
        prefix_down_offset,
        plan_hash,
        plan_ref.clone(),
        published.manifest_ref.clone(),
        vec![prefix_leaf_ref.clone()],
    );
    prefix_down_root_artifact
        .validate_against(&prefix_down_root_spec, &limits)
        .expect("validate GratisPrefixDown root artifact");
    assert!(matches!(
        decode_gratis_prefix_down_output(
            prefix_down_root_artifact.phase_payload(&limits).unwrap(),
            &limits,
        )
        .expect("decode GratisPrefixDown root"),
        GratisPrefixDownOutputV1::Branch([Some(_), None])
    ));

    let mut prefix_down_root_ref = cas
        .publish_bytes(
            &prefix_down_root_artifact
                .encode_canonical(&limits)
                .expect("canonical GratisPrefixDown root artifact"),
        )
        .expect("publish GratisPrefixDown root artifact");
    prefix_down_root_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let prefix_down_leaf_spec = planner
        .gratis_prefix_down_unit_at(
            1,
            &[
                Some(prefix_down_root_artifact.unit_id),
                Some(prefix_leaf_artifact.unit_id),
            ],
            &limits,
        )
        .expect("derive GratisPrefixDown leaf");
    let prefix_down_leaf_artifact = execute_real_worker_unit(
        503,
        0x97,
        &user,
        uid,
        directory.path(),
        cas_limits,
        &inbox_root,
        inbox_limits,
        limits,
        &prefix_down_leaf_spec,
        prefix_down_offset + 1,
        plan_hash,
        plan_ref.clone(),
        published.manifest_ref.clone(),
        vec![prefix_down_root_ref, prefix_leaf_ref],
    );
    prefix_down_leaf_artifact
        .validate_against(&prefix_down_leaf_spec, &limits)
        .expect("validate GratisPrefixDown leaf artifact");
    let GratisPrefixDownOutputV1::Leaf(prefix) = decode_gratis_prefix_down_output(
        prefix_down_leaf_artifact.phase_payload(&limits).unwrap(),
        &limits,
    )
    .expect("decode GratisPrefixDown leaf")
    else {
        panic!("expected GratisPrefixDown leaf output");
    };
    assert_eq!(prefix.segment_ordinal, 0);
    assert_eq!(prefix.incoming_remaining, plan.lysis_budget);
    assert_eq!(
        prefix.outgoing_remaining,
        plan.lysis_budget - amount.checked_segment_gratis_total
    );

    let mut amount_finalize_ref = cas
        .publish_bytes(
            &amount_artifact
                .encode_canonical(&limits)
                .expect("canonical AmountMap finalize producer"),
        )
        .expect("publish AmountMap finalize producer");
    amount_finalize_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let mut prefix_down_leaf_ref = cas
        .publish_bytes(
            &prefix_down_leaf_artifact
                .encode_canonical(&limits)
                .expect("canonical GratisPrefixDown leaf producer"),
        )
        .expect("publish GratisPrefixDown leaf producer");
    prefix_down_leaf_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
    let output_finalize_spec = planner
        .output_finalize_unit_at(
            0,
            &amount_spec,
            prefix_down_leaf_artifact.unit_id,
            &limits,
        )
        .expect("derive OutputFinalize unit");
    let output_finalize_offset = prefix_down_offset
        .checked_add(topology.phase_unit_count(UnitPhase::GratisPrefixDown))
        .expect("OutputFinalize offset");
    let output_finalize_artifact = execute_real_worker_unit(
        504,
        0x99,
        &user,
        uid,
        directory.path(),
        cas_limits,
        &inbox_root,
        inbox_limits,
        limits,
        &output_finalize_spec,
        output_finalize_offset,
        plan_hash,
        plan_ref,
        published.manifest_ref,
        vec![amount_finalize_ref, prefix_down_leaf_ref],
    );
    output_finalize_artifact
        .validate_against(&output_finalize_spec, &limits)
        .expect("validate OutputFinalize artifact");
    let finalized = decode_finalized_output_run(
        output_finalize_artifact.phase_payload(&limits).unwrap(),
        &limits,
    )
    .expect("decode OutputFinalize artifact");
    assert_eq!(finalized.ordered_records.len(), 1);
    assert_eq!(
        finalized.ordered_records[0].nod_action.issued_at,
        plan.logical_evaluation_time
    );
    assert_eq!(
        finalized.ordered_records[0]
            .nod_action
            .source_tribute_id,
        tribute.tribute_id
    );
}

#[allow(clippy::too_many_arguments)]
fn execute_real_worker_unit(
    generation: u64,
    boot: u8,
    user: &str,
    uid: u32,
    cas_root: &std::path::Path,
    cas_limits: CasLimits,
    inbox_root: &std::path::Path,
    inbox_limits: WorkerInboxLimits,
    limits: SchemaLimits,
    spec: &UnitSpecV1,
    unit_index: u32,
    plan_hash: B256,
    plan_ref: CasObjectRefV1,
    input_manifest_ref: CasObjectRefV1,
    ordered_input_refs: Vec<CasObjectRefV1>,
) -> UnitArtifactV1 {
    let (parent_stream, child_stream) = UnixStream::pair().expect("worker socket pair");
    let child_fd: OwnedFd = child_stream.into();
    let worker_identity = identity(boot);
    let mut command = Command::new(env::current_exe().expect("current Rust test binary"));
    command
        .args([
            "--exact",
            "real_worker_processes_execute_through_output_finalize",
            "--nocapture",
        ])
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
            cas_limits.max_object_bytes.to_string(),
        )
        .env(CHILD_CAS_TOTAL_CAP, cas_limits.max_total_bytes.to_string())
        .env(CHILD_INBOX_ROOT, inbox_root)
        .stdin(Stdio::from(child_fd))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn production worker");
    drop(command);

    let client_identity = EndpointIdentity {
        boot_nonce: B256::repeat_byte(boot.wrapping_add(1)),
        ..worker_identity
    };
    let mut client = ControlClientSession::connect(
        parent_stream,
        ClientPolicy::supervisor_to_worker(uid, client_identity, limits),
    )
    .expect("worker client");
    client.handshake().expect("worker handshake");
    client
        .send_request(
            WorkerMessageKind::RunUnit as u16,
            RunUnitV1 {
                protocol_bundle_hash: spec.protocol_bundle_hash,
                job_id: spec.job_id,
                attempt: spec.attempt,
                plan_hash,
                unit_index,
                canonical_unit_spec: BoundedBytes(
                    spec.encode_canonical(&limits)
                        .expect("canonical worker unit spec"),
                ),
                unit_membership_siblings: Vec::new(),
                plan_ref,
                input_manifest_ref,
                ordered_input_refs,
            }
            .encode_body(&limits)
            .expect("worker RunUnit body"),
        )
        .expect("send worker unit");
    let output = child.wait_with_output().expect("worker exit");
    assert!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frame = client.receive_response().expect("worker response");
    let finished = UnitFinishedV1::decode_body(&frame.body, &limits).expect("worker finished body");
    assert_eq!(finished.status, UnitFinishedStatus::Success);
    assert_eq!(
        finished.unit_id,
        spec.unit_id(&limits).expect("worker UnitId")
    );
    let inbox = WorkerInbox::open(inbox_root, inbox_limits).expect("open worker inbox");
    UnitArtifactV1::decode_canonical(
        inbox
            .read_reported(
                finished.unit_id,
                finished.exact_staged_bytes,
                finished.transport_digest,
            )
            .expect("read staged worker artifact")
            .bytes(),
        &limits,
    )
    .expect("decode worker artifact")
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
    let user = env::var(CHILD_USER).expect("worker child user");
    let limits = poc_schema_limits();
    let expected_bundle_hash = parse_b256(CHILD_BUNDLE);
    let canonical_bundle = support::protocol_bundle()
        .encode_canonical(&limits)
        .expect("canonical child protocol bundle");
    run_one_from_inherited_fd(WorkerConfig {
        expected_effective_user: user.clone(),
        expected_supervisor_user: user,
        identity: EndpointIdentity {
            chain_id: parse_u64(CHILD_CHAIN_ID),
            genesis_hash: parse_b256(CHILD_GENESIS),
            boot_nonce: parse_b256(CHILD_BOOT_NONCE),
            protocol_bundle_hash: expected_bundle_hash,
        },
        session_generation: parse_u64(CHILD_GENERATION),
        cas_root: PathBuf::from(env::var_os(CHILD_CAS_ROOT).expect("worker child CAS root")),
        cas_limits: CasLimits {
            max_object_bytes: parse_u64(CHILD_CAS_OBJECT_CAP),
            max_total_bytes: parse_u64(CHILD_CAS_TOTAL_CAP),
        },
        inbox_root: PathBuf::from(env::var_os(CHILD_INBOX_ROOT).expect("worker child inbox root")),
        inbox_limits: WorkerInboxLimits {
            max_artifact_bytes: 1024 * 1024,
            max_total_bytes: 4 * 1024 * 1024,
        },
        connection_fd: 0,
        protocol_bundle: PinnedProtocolBundle::decode(
            &canonical_bundle,
            expected_bundle_hash,
            &limits,
        )
        .expect("pin child protocol bundle"),
    })
    .expect("production worker function");
}
