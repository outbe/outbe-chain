use super::{ExpectedRpcCall, RecordedRpcCall, RecordedRpcResponse, RecordingRpc};
use crate::abi::{ITeeRegistry, TEE_REGISTRY_ADDR};
use crate::commands::tee::TeeCmd;
use crate::errors::TeeIdentityError;
use alloy_primitives::{B256, U256};
use alloy_sol_types::SolCall;
use clap::Parser;
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, EnclaveInitializationManifestV1, NodeIdV1,
};
use outbe_tee::codec::{decode_call, decode_request, encode_response, read_frame, write_frame};
use outbe_tee::node_host::{NODE_HOST_DIRECTORY_V1, NODE_HOST_MANIFEST_V1, NODE_HOST_NOISE_KEY_V1};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::NodeHostNoiseKey;
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const RESIDENT_OFFER_KEY: [u8; 32] = [0x61; 32];

fn command(endpoint: &str, node_data_dir: &Path, diff_chain: bool) -> TeeCmd {
    let mut arguments = vec![
        "outbe-cli",
        "tee",
        "pubkey",
        "--enclave-socket",
        endpoint,
        "--node-data-dir",
        node_data_dir.to_str().unwrap(),
    ];
    if diff_chain {
        arguments.push("--diff-chain");
    }
    let crate::Commands::Tee { cmd } = crate::Cli::try_parse_from(arguments).unwrap().command
    else {
        panic!("expected tee command")
    };
    cmd
}

fn committed_identity(node_data_dir: &Path, noise_public: [u8; 32]) -> [u8; 32] {
    let root = node_data_dir.join(NODE_HOST_DIRECTORY_V1);
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let node_host = NodeHostNoiseKey::create_new(&root.join(NODE_HOST_NOISE_KEY_V1)).unwrap();
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x31; 32]).into()).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(54_322_345_u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x32),
        attestation_mode: AttestationMode::GramineDirectDev,
        node_id: NodeIdV1 {
            reth_p2p_public: node_signer
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        initialization_challenge: [0x33; 32],
        node_host_noise_x25519: node_host.public(),
        recipient_x25519: [0x34; 32],
        attestation_ed25519: [0x35; 32],
        noise_responder_x25519: noise_public,
    };
    let path = root.join(NODE_HOST_MANIFEST_V1);
    fs::write(&path, manifest.encode_canonical().unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    node_host.public()
}

fn serve_public_keys(
    listener: TcpListener,
    responder_private: Vec<u8>,
    responder_public: [u8; 32],
    expected_node_host: [u8; 32],
    ready: bool,
) -> eyre::Result<()> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                eyre::ensure!(
                    Instant::now() < deadline,
                    "CLI did not connect to the enclave"
                );
                thread::park_timeout(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    };
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // The old CLI sent GetQuote here and failed before the authenticated session.
    assert!(matches!(
        decode_request(&read_frame(&mut stream)?)?,
        EnclaveRequest::OpenSession
    ));
    let mut handshake = snow::Builder::new(outbe_tee::NOISE_PARAMS.parse()?)
        .local_private_key(&responder_private)
        .build_responder()?;
    let mut buffer = [0; 1024];
    handshake.read_message(&read_frame(&mut stream)?, &mut buffer)?;
    assert_eq!(
        handshake.get_remote_static(),
        Some(expected_node_host.as_slice())
    );
    let length = handshake.write_message(&[], &mut buffer)?;
    write_frame(&mut stream, &buffer[..length])?;
    let mut session = handshake.into_transport_mode()?;
    let length = session.read_message(&read_frame(&mut stream)?, &mut buffer)?;
    assert!(matches!(
        decode_call(&buffer[..length])?.request,
        EnclaveRequest::GetPublicKeys
    ));
    let response = encode_response(&EnclaveResponse::PublicKeys {
        offer_key_ready: ready,
        recipient_x25519_pub: if ready {
            RESIDENT_OFFER_KEY
        } else {
            [0x34; 32]
        },
        attestation_pub: [0x35; 32],
        noise_static_pub: responder_public,
        tee_bls_pub: vec![0x36; 48],
        dkg_enc_pub: [0x37; 32],
        dkg_enc_sig: Vec::new(),
    })?;
    let length = session.write_message(&response, &mut buffer)?;
    write_frame(&mut stream, &buffer[..length])?;
    Ok(())
}

#[tokio::test]
async fn pubkey_authenticates_committed_node_host_and_preserves_diff_contract() {
    enum Failure {
        NotReady,
        NotBootstrapped,
        Mismatch,
    }
    let cases = [
        ("ready without diff", true, false, None, None),
        ("keyless without diff", false, false, None, None),
        (
            "matching registry",
            true,
            true,
            Some(RESIDENT_OFFER_KEY),
            None,
        ),
        (
            "mismatching registry",
            true,
            true,
            Some([0x71; 32]),
            Some(Failure::Mismatch),
        ),
        (
            "unbootstrapped registry",
            true,
            true,
            Some([0; 32]),
            Some(Failure::NotBootstrapped),
        ),
        ("onboarding key", false, true, None, Some(Failure::NotReady)),
    ];
    for (label, ready, diff_chain, registry_key, failure) in cases {
        let directory = tempfile::tempdir().unwrap();
        let keys = snow::Builder::new(outbe_tee::NOISE_PARAMS.parse().unwrap())
            .generate_keypair()
            .unwrap();
        let responder_public = keys.public.try_into().unwrap();
        let node_host = committed_identity(directory.path(), responder_public);
        let root = directory.path().join(NODE_HOST_DIRECTORY_V1);
        let key_before = fs::read(root.join(NODE_HOST_NOISE_KEY_V1)).unwrap();
        let manifest_before = fs::read(root.join(NODE_HOST_MANIFEST_V1)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            serve_public_keys(listener, keys.private, responder_public, node_host, ready)
        });
        let rpc = RecordingRpc::new(registry_key.map(|key| {
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthCall {
                    to: TEE_REGISTRY_ADDR,
                    data: ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
                },
                RecordedRpcResponse::Bytes(key.to_vec()),
            )
        }));
        let result = command(&endpoint, directory.path(), diff_chain)
            .run(&rpc, None)
            .await;
        server.join().unwrap().unwrap();
        match failure {
            None => assert!(result.is_ok(), "{label}: {result:?}"),
            Some(expected) => {
                let error = result.unwrap_err();
                let error = error.downcast_ref::<TeeIdentityError>().unwrap();
                assert!(
                    matches!(
                        (expected, error),
                        (
                            Failure::NotReady,
                            TeeIdentityError::PermanentOfferKeyNotReady
                        ) | (
                            Failure::NotBootstrapped,
                            TeeIdentityError::ChainNotTeeBootstrapped
                        ) | (Failure::Mismatch, TeeIdentityError::OfferKeyMismatch { .. })
                    ),
                    "{label}: {error}"
                );
            }
        }
        rpc.assert_done();
        assert_eq!(
            fs::read(root.join(NODE_HOST_NOISE_KEY_V1)).unwrap(),
            key_before
        );
        assert_eq!(
            fs::read(root.join(NODE_HOST_MANIFEST_V1)).unwrap(),
            manifest_before
        );
    }
}

#[tokio::test]
async fn pubkey_requires_committed_identity_without_implicit_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let endpoint = directory.path().join("unused.sock");
    let rpc = RecordingRpc::new([]);
    let error = command(endpoint.to_str().unwrap(), directory.path(), true)
        .run(&rpc, None)
        .await
        .unwrap_err();
    let Some(TeeIdentityError::ConnectEnclave { source, .. }) = error.downcast_ref() else {
        panic!("expected an identity error: {error}")
    };
    assert!(source
        .to_string()
        .contains("committed production NodeHost manifest"));
    let root = directory.path().join(NODE_HOST_DIRECTORY_V1);
    assert!(!root.join(NODE_HOST_NOISE_KEY_V1).exists());
    assert!(!root.join(NODE_HOST_MANIFEST_V1).exists());
    rpc.assert_done();
    assert!(rpc.recorded_calls().is_empty());
}
