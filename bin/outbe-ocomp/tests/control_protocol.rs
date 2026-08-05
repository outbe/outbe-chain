// OCOMP-TEST-ID: OCM-CTL-001

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

use alloy_primitives::B256;
use outbe_ocomp::control::{
    poc_schema_limits, ClientPolicy, ControlClientSession, ControlError, ControlRole,
    ControlServerSession, EndpointIdentity, ServerPolicy,
};
use outbe_ocomp::snapshot_client::{
    SnapshotClientError, SnapshotExporterNodeClient, SnapshotExporterNodeConfig,
};
use outbe_ocomp_protocol::{
    ControlFrameV1, ControlMagic, ControlRoleV1, HelloAckV1, ListSnapshotHandoffsResponseV1,
    NodeMessageKind, WorkerMessageKind, CONTROL_FRAME_LEN_AFTER_PREFIX,
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
    let (client_stream, server_stream) = connected_pair();
    let server_identity = identity(0x51);
    let client_identity = identity(0x52);

    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::node(ControlRole::Supervisor, server_identity, 7, limits),
        )
        .expect("kernel peer credentials");
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
        ClientPolicy::supervisor_to_node(client_identity, limits),
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
fn ocm_ctl_001_snapshot_exporter_transports_a_maximum_bounded_protocol_response() {
    let limits = poc_schema_limits();
    let (client_stream, server_stream) = connected_pair();
    let expected_body = vec![0xA5; limits.max_bounded_bytes];

    let server_body = expected_body.clone();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::node(ControlRole::SnapshotExporter, identity(0x53), 8, limits),
        )
        .expect("peer credentials");
        session.handshake().expect("compatible exporter hello");
        let request = session.receive_request().expect("bounded opening request");
        assert_eq!(
            request.message_kind,
            NodeMessageKind::BuildLysisOpenings as u16
        );
        session
            .send_response(
                request.request_id,
                NodeMessageKind::Response as u16,
                server_body,
            )
            .expect("maximum protocol-bounded response must fit local control");
    });

    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::exporter_to_node(identity(0x54), limits),
    )
    .expect("client session");
    let negotiated = client.handshake().expect("handshake");
    assert!(
        negotiated.max_control_body_bytes >= limits.max_bounded_bytes,
        "negotiated local-control cap must carry every protocol-valid body"
    );
    client
        .send_request(NodeMessageKind::BuildLysisOpenings as u16, Vec::new())
        .expect("request bounded opening");
    let response = client
        .receive_response()
        .expect("receive maximum bounded protocol response");
    assert_eq!(response.message_kind, NodeMessageKind::Response as u16);
    assert_eq!(response.body, expected_body);
    server.join().expect("server thread");
}

#[test]
fn ocm_ctl_001_snapshot_client_reconnects_after_a_dropped_response() {
    let limits = poc_schema_limits();
    let directory = tempdir().expect("socket directory");
    let socket = directory.path().join("reconnect.sock");
    let listener = UnixListener::bind(&socket).expect("bind control socket");
    let server = thread::spawn(move || {
        for session_generation in [9_u64, 10] {
            let (stream, _) = listener.accept().expect("accept control socket");
            let mut session = ControlServerSession::accept(
                stream,
                ServerPolicy::node(
                    ControlRole::SnapshotExporter,
                    identity(0x55),
                    session_generation,
                    limits,
                ),
            )
            .expect("peer credentials");
            session.handshake().expect("compatible exporter hello");
            let request = session.receive_request().expect("list request");
            assert_eq!(
                request.message_kind,
                NodeMessageKind::ListSnapshotHandoffs as u16
            );
            if session_generation == 9 {
                continue;
            }
            let response = ListSnapshotHandoffsResponseV1 {
                next_lease_generation: 0,
                handoffs: Vec::new(),
            };
            session
                .send_response(
                    request.request_id,
                    NodeMessageKind::Response as u16,
                    response.encode_body(&limits).expect("encode list response"),
                )
                .expect("send list response");
        }
    });

    let mut client = SnapshotExporterNodeClient::connect(&SnapshotExporterNodeConfig {
        node_socket: socket,
        identity: identity(0x56),
        limits,
    })
    .expect("initial client session");
    assert!(matches!(
        client.list(0).expect_err("first response is dropped"),
        SnapshotClientError::Control(ControlError::ConnectionClosed)
    ));
    assert_eq!(
        client.list(0).expect("next call reconnects"),
        ListSnapshotHandoffsResponseV1 {
            next_lease_generation: 0,
            handoffs: Vec::new(),
        }
    );
    server.join().expect("server thread");
}

#[test]
fn ocm_ctl_001_bundle_role_and_method_fail_closed() {
    let limits = poc_schema_limits();

    let (client_stream, server_stream) = connected_pair();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::node(ControlRole::SnapshotExporter, identity(0x62), 10, limits),
        )
        .expect("peer credentials");
        session.handshake().expect_err("wrong bundle rejected")
    });
    let mut wrong_bundle = identity(0x63);
    wrong_bundle.protocol_bundle_hash = B256::repeat_byte(0xEE);
    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::exporter_to_node(wrong_bundle, limits),
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
            ServerPolicy::node(ControlRole::SnapshotExporter, identity(0x64), 11, limits),
        )
        .expect("peer credentials");
        session.handshake().expect("handshake");
        session
            .receive_request()
            .expect_err("exporter cannot request attestation")
    });
    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::exporter_to_node(identity(0x65), limits),
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
    let (mut client_stream, server_stream) = connected_pair();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::worker(identity(0x71), 12, limits),
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
    let (client_stream, server_stream) = connected_pair();
    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::worker(identity(0x81), 13, limits),
        )
        .expect("peer credentials");
        session.handshake().expect("worker handshake");
        session
            .receive_request()
            .expect_err("node kind rejected on worker control")
    });
    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::supervisor_to_worker(identity(0x82), limits),
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
        ClientPolicy::supervisor_to_node(expected, limits),
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
