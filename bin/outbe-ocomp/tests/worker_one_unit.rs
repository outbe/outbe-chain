use std::env;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use alloy_primitives::B256;
use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
use outbe_ocomp::control::{
    effective_uid, effective_user_name, poc_schema_limits, ClientPolicy, ControlClientSession,
    EndpointIdentity,
};
use outbe_ocomp::worker::{run_one_from_inherited_fd, WorkerConfig};
use outbe_ocomp_protocol::common::{BoundedBytes, EntityId36};
use outbe_ocomp_protocol::unit::{EntityIdHalfOpenRange, UnitInterval, UnitPhase, UnitSpecV1};
use outbe_ocomp_protocol::{RunUnitV1, UnitFinishedStatus, UnitFinishedV1, WorkerMessageKind};
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

fn identity(boot: u8) -> EndpointIdentity {
    EndpointIdentity {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        boot_nonce: B256::repeat_byte(boot),
        protocol_bundle_hash: B256::repeat_byte(0x91),
    }
}

#[test]
fn four_real_worker_processes_each_handle_one_exact_unit_and_exit() {
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
    let plan_ref = cas.publish_bytes(b"fixed plan bytes").expect("plan object");
    let manifest_ref = cas
        .publish_bytes(b"authenticated manifest bytes")
        .expect("manifest object");
    let input_ref = cas
        .publish_bytes(b"authenticated input bytes")
        .expect("input object");
    let limits = poc_schema_limits();
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
                "four_real_worker_processes_each_handle_one_exact_unit_and_exit",
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

        let spec = UnitSpecV1 {
            protocol_bundle_hash: worker_identity.protocol_bundle_hash,
            job_id: B256::repeat_byte(0x31),
            attempt: 1,
            phase: UnitPhase::Enumerate,
            interval: UnitInterval::EntityIdRange(EntityIdHalfOpenRange {
                start: EntityId36([index; 36]),
                end: Some(EntityId36([index + 1; 36])),
            }),
            canonical_ordered_inputs: Vec::new(),
            lysis_program_semantics_hash: B256::repeat_byte(0x71),
            planner_spec_version: 1,
            reducer_spec_version: 1,
        };
        let unit_id = spec.unit_id(&limits).expect("unit id");
        let request = RunUnitV1 {
            protocol_bundle_hash: spec.protocol_bundle_hash,
            job_id: spec.job_id,
            attempt: spec.attempt,
            plan_hash: B256::repeat_byte(0x72),
            unit_index: u32::from(index),
            canonical_unit_spec: BoundedBytes(
                spec.encode_canonical(&limits).expect("unit spec bytes"),
            ),
            plan_ref: plan_ref.clone(),
            input_manifest_ref: manifest_ref.clone(),
            ordered_input_refs: vec![input_ref.clone()],
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

    for mut worker in workers {
        let frame = worker.client.receive_response().expect("worker response");
        assert_eq!(frame.message_kind, WorkerMessageKind::UnitFinished as u16);
        let finished = UnitFinishedV1::decode_body(&frame.body, &limits).expect("finished body");
        assert_eq!(finished.unit_id, worker.unit_id);
        assert_eq!(finished.status, UnitFinishedStatus::Failed);
        assert_eq!(finished.exact_staged_bytes, 0);
        assert_eq!(finished.transport_digest, B256::ZERO);
        let output = worker.child.wait_with_output().expect("worker exit");
        assert!(
            output.status.success(),
            "worker failed: {} (expected peer uid {uid})",
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
    run_one_from_inherited_fd(WorkerConfig {
        expected_effective_user: user.clone(),
        expected_supervisor_user: user,
        identity: EndpointIdentity {
            chain_id: parse_u64(CHILD_CHAIN_ID),
            genesis_hash: parse_b256(CHILD_GENESIS),
            boot_nonce: parse_b256(CHILD_BOOT_NONCE),
            protocol_bundle_hash: parse_b256(CHILD_BUNDLE),
        },
        session_generation: parse_u64(CHILD_GENERATION),
        cas_root: PathBuf::from(env::var_os(CHILD_CAS_ROOT).expect("worker child CAS root")),
        cas_limits: CasLimits {
            max_object_bytes: parse_u64(CHILD_CAS_OBJECT_CAP),
            max_total_bytes: parse_u64(CHILD_CAS_TOTAL_CAP),
        },
        connection_fd: 0,
    })
    .expect("production worker function");
}
