use std::os::unix::net::UnixStream;
use std::thread;

use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    local_control::{
        effective_uid, ClientPolicy, ControlClientSession, ControlRole, ControlServerSession,
        EndpointIdentity, ServerPolicy,
    },
    profile::poc_schema_limits,
    LocalErrorCode, LocalErrorV1, NodeMessageKind,
};

use crate::ocomp::control::send_bounded_response;

fn identity(byte: u8) -> EndpointIdentity {
    EndpointIdentity {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        boot_nonce: B256::repeat_byte(byte),
        protocol_bundle_hash: B256::repeat_byte(0x91),
    }
}

#[test]
fn ocm_ctl_001_oversized_node_response_is_typed_and_session_remains_usable() {
    let limits = poc_schema_limits();
    let uid = effective_uid().expect("effective uid");
    let (client_stream, server_stream) = UnixStream::pair().expect("local control socket pair");

    let server = thread::spawn(move || {
        let mut session = ControlServerSession::accept(
            server_stream,
            ServerPolicy::node(
                ControlRole::SnapshotExporter,
                uid,
                identity(0x51),
                7,
                limits,
            ),
        )
        .expect("server session");
        session.handshake().expect("server handshake");

        let first = session.receive_request().expect("first request");
        send_bounded_response(
            &mut session,
            first.request_id,
            first.message_kind,
            vec![0xA5; limits.max_control_body_bytes + 1],
            &limits,
        )
        .expect("typed limit response");

        let second = session.receive_request().expect("second request");
        send_bounded_response(
            &mut session,
            second.request_id,
            second.message_kind,
            vec![0x5A],
            &limits,
        )
        .expect("bounded response");
    });

    let mut client = ControlClientSession::connect(
        client_stream,
        ClientPolicy::exporter_to_node(uid, identity(0x52), limits),
    )
    .expect("client session");
    client.handshake().expect("client handshake");

    client
        .send_request(NodeMessageKind::BuildLysisOpenings as u16, Vec::new())
        .expect("first request");
    let rejected = client.receive_response().expect("typed limit response");
    assert_eq!(rejected.message_kind, NodeMessageKind::Error as u16);
    let error = LocalErrorV1::decode_body(&rejected.body, &limits).expect("decode local error");
    assert_eq!(
        error.rejected_kind,
        NodeMessageKind::BuildLysisOpenings as u16
    );
    assert_eq!(error.error_code, LocalErrorCode::LimitExceeded as u16);
    assert!(
        !error.retryable,
        "the exact oversized request is not retryable"
    );

    client
        .send_request(NodeMessageKind::BuildLysisOpenings as u16, Vec::new())
        .expect("second request on same session");
    let response = client.receive_response().expect("bounded response");
    assert_eq!(response.message_kind, NodeMessageKind::Response as u16);
    assert_eq!(response.body, vec![0x5A]);

    server.join().expect("server thread");
}
