use super::*;

#[test]
fn authorized_node_host_client_signs_dev_evidence_in_sgx_no_attest_mode() {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, RegistrationIntentV1};
    use outbe_tee::{AuthorizedEnclaveClient, NodeHostNoiseKey};

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("enclave.sock");
    let endpoint = socket.to_str().unwrap().to_string();
    let boot = Arc::new(EnclaveBootConfig::new(
        testnet_chain_word(),
        root.path().to_path_buf(),
        0,
    ));
    let keys = Arc::new(EnclaveKeys::new([0x53; 32], Some([0x53; 32])).unwrap());
    let initialization = Arc::new(
        crate::initialization::factory::production_with_challenge_and_attestation(
            boot.clone(),
            &keys,
            [0x54; 32],
            crate::gramine::AttestationType::SgxNoAttest,
        )
        .unwrap(),
    );
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let listener = UnixListener::bind(&socket).unwrap();
    let server_keys = keys.clone();
    let server_initialization = initialization.clone();
    let server = std::thread::spawn(move || {
        serve_sequential_unix_connections(
            listener,
            SequentialTestServerContext {
                keys: &server_keys,
                offer_key: &offer_key,
                boot: &boot,
                initialization: &server_initialization,
            },
            2,
            serve_connection_with::<UnixStream>,
        );
    });

    let challenge = AuthorizedEnclaveClient::discover_endpoint(&endpoint).unwrap();
    let node_host_path = root.path().join("node-host-noise.key");
    let node_host = NodeHostNoiseKey::create_new(&node_host_path).unwrap();
    let (manifest, node_signature) = signed_initialization_manifest_for_mode(
        &keys,
        challenge.challenge,
        node_host.public(),
        AttestationMode::NoAttestationDev,
    );
    let mut client = AuthorizedEnclaveClient::initialize_endpoint(
        &endpoint,
        &manifest,
        &node_signature,
        &node_host,
    )
    .unwrap();
    let intent = RegistrationIntentV1 {
        attestation_mode: AttestationMode::NoAttestationDev,
        policy_hash: B256::repeat_byte(0x55),
        binding_id: B256::repeat_byte(0x56),
        ..crate::initialization::test_support::registration_intent_for_manifest(&manifest)
    };
    let signature = client.sign_registration_intent_dev_v1(&intent).unwrap();
    assert!(intent.verify_enclave_signature(&signature));
    drop(client);
    server.join().unwrap();
}

#[test]
fn production_initialization_binds_noise_initiator_before_request_decode() {
    use outbe_tee::codec::{decode_response, encode_request};

    let root = tempfile::tempdir().unwrap();
    let boot = Arc::new(EnclaveBootConfig::new(
        testnet_chain_word(),
        root.path().to_path_buf(),
        0,
    ));
    let keys = Arc::new(EnclaveKeys::new([7; 32], Some([1; 32])).unwrap());
    let initialization = Arc::new(production_dcap_state(boot.clone(), &keys));
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());

    let challenge = match initialization.challenge_response(&keys).unwrap() {
        EnclaveResponse::InitializationChallenge { challenge, .. } => challenge,
        response => panic!("unexpected challenge response: {response:?}"),
    };
    let node_host_private = [0x51; 32];
    let node_host_public =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(node_host_private))
            .to_bytes();
    let (manifest, node_signature) =
        signed_initialization_manifest(&keys, challenge, node_host_public);

    // First connection: signed manifest plus possession of the exact embedded
    // NodeHost Noise key commits the write-once authorization.
    let (mut client, server) = spawn_production_connection(
        keys.clone(),
        boot.clone(),
        offer_key.clone(),
        initialization.clone(),
    );
    write_frame(
        &mut client,
        &encode_request(&EnclaveRequest::Initialize {
            manifest: manifest.encode_canonical().unwrap(),
            node_signature: node_signature.to_vec(),
        })
        .unwrap(),
    )
    .unwrap();
    let params = NOISE_PARAMS.parse().unwrap();
    let mut handshake = snow::Builder::new(params)
        .local_private_key(&node_host_private)
        .remote_public_key(&keys.noise_public())
        .build_initiator()
        .unwrap();
    let mut buf = [0u8; 2048];
    let len = handshake.write_message(&[], &mut buf).unwrap();
    write_frame(&mut client, &buf[..len]).unwrap();
    let msg2 = read_frame(&mut client).unwrap();
    handshake.read_message(&msg2, &mut buf).unwrap();
    let mut noise = handshake.into_transport_mode().unwrap();
    let frame = read_frame(&mut client).unwrap();
    let len = noise.read_message(&frame, &mut buf).unwrap();
    assert!(matches!(
        decode_response(&buf[..len]).unwrap(),
        EnclaveResponse::Initialized {
            sealed_loaded: false,
            ..
        }
    ));
    drop(client);
    server.join().unwrap().unwrap();
    assert_eq!(
        initialization.expected_node_host().unwrap(),
        node_host_public
    );

    // The authorized persistent key can reconnect and issue an allowed
    // command through the production channel.
    let (mut client, server) = spawn_production_connection(
        keys.clone(),
        boot.clone(),
        offer_key.clone(),
        initialization.clone(),
    );
    write_frame(
        &mut client,
        &encode_request(&EnclaveRequest::OpenSession).unwrap(),
    )
    .unwrap();
    let params = NOISE_PARAMS.parse().unwrap();
    let mut handshake = snow::Builder::new(params)
        .local_private_key(&node_host_private)
        .remote_public_key(&keys.noise_public())
        .build_initiator()
        .unwrap();
    let len = handshake.write_message(&[], &mut buf).unwrap();
    write_frame(&mut client, &buf[..len]).unwrap();
    let msg2 = read_frame(&mut client).unwrap();
    handshake.read_message(&msg2, &mut buf).unwrap();
    let mut noise = handshake.into_transport_mode().unwrap();
    let request = encode_request(&EnclaveRequest::GetPublicKeys).unwrap();
    let len = noise.write_message(&request, &mut buf).unwrap();
    write_frame(&mut client, &buf[..len]).unwrap();
    let frame = read_frame(&mut client).unwrap();
    let len = noise.read_message(&frame, &mut buf).unwrap();
    assert!(matches!(
        decode_response(&buf[..len]).unwrap(),
        EnclaveResponse::PublicKeys { .. }
    ));
    drop(client);
    server.join().unwrap().unwrap();

    // An unknown initiator is rejected after Noise message 1 even when that
    // message carries bytes that are not a valid EnclaveRequest. The server
    // never writes message 2 and therefore cannot decode or dispatch them.
    let (mut attacker, server) =
        spawn_production_connection(keys.clone(), boot, offer_key, initialization.clone());
    write_frame(
        &mut attacker,
        &encode_request(&EnclaveRequest::OpenSession).unwrap(),
    )
    .unwrap();
    let attacker_private = [0xA5; 32];
    let params = NOISE_PARAMS.parse().unwrap();
    let mut attacker_handshake = snow::Builder::new(params)
        .local_private_key(&attacker_private)
        .remote_public_key(&keys.noise_public())
        .build_initiator()
        .unwrap();
    let len = attacker_handshake
        .write_message(&[0xff; 32], &mut buf)
        .unwrap();
    write_frame(&mut attacker, &buf[..len]).unwrap();
    assert!(read_frame(&mut attacker).is_err());
    let error = server.join().unwrap().unwrap_err();
    assert!(error
        .to_string()
        .contains("initiator is not the authorized NodeHost"));
    assert_eq!(
        initialization.expected_node_host().unwrap(),
        node_host_public
    );
}
