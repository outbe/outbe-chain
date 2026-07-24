// OCOMP-TEST-ID: OCM-CTL-001

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

use alloy_primitives::B256;
use outbe_ocomp::control::{
    effective_uid, poc_schema_limits, ClientPolicy, ControlClientSession, ControlError,
    ControlRole, ControlServerSession, EndpointIdentity, ServerPolicy,
};
use outbe_ocomp_protocol::{
    ControlFrameV1, ControlMagic, ControlRoleV1, HelloAckV1, NodeMessageKind, WorkerMessageKind,
    CONTROL_FRAME_LEN_AFTER_PREFIX,
};
use tempfile::tempdir;

fn identity(byte: u8) -> EndpointIdentity {
    EndpointIdentity {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        boot_nonce: B256::repeat_byte(byte),
        protocol_bundle_hash: B256::repeat_byte(0x91),
    }
}

fn connected_pair() -> (UnixStream, UnixStream) {
    let directory = tempdir().expect("socket directory");
    let socket = directory.path().join("control.sock");
    let listener = UnixListener::bind(&socket).expect("bind control socket");
    let client = UnixStream::connect(&socket).expect("connect control socket");
    let (server, _) = listener.accept().expect("accept control socket");
    (client, server)
}

#[test]
fn ocm_ctl_001_real_peer_credentials_bundle_acl_and_counter_are_enforced() {
    let limits = poc_schema_limits();
    let uid = effective_uid().expect("effective uid");
    let (client_stream, server_stream) = connected_pair();
    let server_identity = identity(0x51);
    let client_identity = identity(0x52);

    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::node(ControlRole::Supervisor, uid, server_identity, 7, limits),
        )
        .expect("kernel peer credentials");
        assert_eq!(session.peer_credentials().uid, uid);
        assert!(session.peer_credentials().pid > 0);
        session.handshake().expect("compatible hello");

        let first = session.receive_request().expect("first request");
        assert_eq!(
            first.message_kind,
            NodeMessageKind::ListFinalizedJobs as u16
        );
        let replay = session.receive_request().expect_err("replay rejected");
        assert!(matches!(
            replay,
            ControlError::UnexpectedRequestId {
                expected: 2,
                actual: 1
            }
        ));
    });

    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::supervisor_to_node(uid, client_identity, limits),
    )
    .expect("client session");
    let negotiated = client.handshake().expect("handshake");
    assert_eq!(negotiated.session_generation, 7);
    client
        .send_request(NodeMessageKind::ListFinalizedJobs as u16, Vec::new())
        .expect("authorized request");
    client
        .send_request_with_id(NodeMessageKind::ListFinalizedJobs as u16, 1, Vec::new())
        .expect("wire replay for server rejection");
    server.join().expect("server thread");
}

#[test]
fn ocm_ctl_001_wrong_uid_bundle_role_and_method_fail_closed() {
    let limits = poc_schema_limits();
    let uid = effective_uid().expect("effective uid");

    let (_, wrong_uid_server) = connected_pair();
    let error = ControlServerSession::accept(
        wrong_uid_server,
        ServerPolicy::worker(uid.saturating_add(1), identity(0x61), 9, limits),
    )
    .expect_err("wrong uid rejected from SO_PEERCRED");
    assert!(matches!(error, ControlError::UnauthorizedPeer { .. }));

    let (wrong_server_client, _wrong_server) = connected_pair();
    let error = ControlClientSession::connect(
        wrong_server_client,
        ClientPolicy::supervisor_to_node(uid.saturating_add(1), identity(0x60), limits),
    )
    .expect_err("wrong server uid rejected from SO_PEERCRED");
    assert!(matches!(error, ControlError::UnauthorizedPeer { .. }));

    let (client_stream, server_stream) = connected_pair();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::node(
                ControlRole::SnapshotExporter,
                uid,
                identity(0x62),
                10,
                limits,
            ),
        )
        .expect("peer credentials");
        session.handshake().expect_err("wrong bundle rejected")
    });
    let mut wrong_bundle = identity(0x63);
    wrong_bundle.protocol_bundle_hash = B256::repeat_byte(0xEE);
    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::exporter_to_node(uid, wrong_bundle, limits),
    )
    .expect("client session");
    assert!(matches!(
        client.handshake().expect_err("no common bundle"),
        ControlError::RemoteRejected { .. } | ControlError::ConnectionClosed
    ));
    assert!(matches!(
        server.join().expect("server thread"),
        ControlError::NoCommonBundle
    ));

    let (client_stream, server_stream) = connected_pair();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::node(
                ControlRole::SnapshotExporter,
                uid,
                identity(0x64),
                11,
                limits,
            ),
        )
        .expect("peer credentials");
        session.handshake().expect("handshake");
        session
            .receive_request()
            .expect_err("exporter cannot request attestation")
    });
    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::exporter_to_node(uid, identity(0x65), limits),
    )
    .expect("client session");
    client.handshake().expect("handshake");
    client
        .send_request(NodeMessageKind::RequestAttestation as u16, Vec::new())
        .expect("wire unauthorized method");
    assert!(matches!(
        server.join().expect("server thread"),
        ControlError::UnauthorizedMethod { .. }
    ));
}

#[test]
fn ocm_ctl_001_cap_plus_one_is_rejected_from_prefix_before_body_read() {
    let limits = poc_schema_limits();
    let uid = effective_uid().expect("effective uid");
    let (mut client_stream, server_stream) = connected_pair();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::worker(uid, identity(0x71), 12, limits),
        )
        .expect("peer credentials");
        session
            .receive_raw_frame()
            .expect_err("oversize prefix rejected")
    });

    let cap_plus_one =
        u32::try_from(CONTROL_FRAME_LEN_AFTER_PREFIX + limits.max_control_body_bytes + 1)
            .expect("PoC frame cap fits u32");
    client_stream
        .write_all(&cap_plus_one.to_be_bytes())
        .expect("write only frame prefix");
    assert!(matches!(
        server.join().expect("server thread"),
        ControlError::FrameTooLarge { .. }
    ));
}

#[test]
fn ocm_ctl_001_worker_magic_rejects_node_messages() {
    let limits = poc_schema_limits();
    let uid = effective_uid().expect("effective uid");
    let (client_stream, server_stream) = connected_pair();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::worker(uid, identity(0x81), 13, limits),
        )
        .expect("peer credentials");
        session.handshake().expect("worker handshake");
        session
            .receive_request()
            .expect_err("node kind rejected on worker control")
    });
    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::supervisor_to_worker(uid, identity(0x82), limits),
    )
    .expect("client session");
    client.handshake().expect("worker handshake");
    client
        .send_request(NodeMessageKind::GetJobSpec as u16, Vec::new())
        .expect("wire invalid kind");
    assert!(matches!(
        server.join().expect("server thread"),
        ControlError::UnauthorizedMethod { .. }
    ));

    assert_ne!(ControlMagic::Node, ControlMagic::Worker);
    assert_eq!(WorkerMessageKind::RunUnit as u16, 0x0010);
}

#[test]
fn ocm_ctl_001_client_rejects_server_capability_negotiation() {
    let limits = poc_schema_limits();
    let uid = effective_uid().expect("effective uid");
    let (client_stream, mut server_stream) = connected_pair();
    let expected = identity(0x91);
    let server = thread::spawn(move || {
        let mut prefix = [0_u8; 4];
        server_stream
            .read_exact(&mut prefix)
            .expect("read hello prefix");
        let frame_len = usize::try_from(u32::from_be_bytes(prefix)).expect("frame length");
        let mut rest = vec![0_u8; frame_len];
        server_stream.read_exact(&mut rest).expect("read hello");

        let ack = HelloAckV1 {
            role: ControlRoleV1::Node,
            server_boot_nonce: B256::repeat_byte(0x92),
            protocol_bundle_hash: expected.protocol_bundle_hash,
            capability_bits: u64::MAX,
            max_control_body_bytes: u32::try_from(limits.max_control_body_bytes)
                .expect("control cap fits u32"),
            session_generation: 14,
        };
        let frame = ControlFrameV1 {
            magic: ControlMagic::Node,
            message_kind: NodeMessageKind::HelloAck as u16,
            session_generation: 0,
            request_id: 0,
            body: ack.encode_body(&limits).expect("encode hostile ack"),
        };
        server_stream
            .write_all(&frame.encode(&limits).expect("encode ack frame"))
            .expect("write hostile ack");
    });

    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::supervisor_to_node(uid, expected, limits),
    )
    .expect("server peer uid");
    assert!(matches!(
        client
            .handshake()
            .expect_err("server cannot negotiate capabilities"),
        ControlError::CapabilityEscalation
    ));
    server.join().expect("server thread");
}
