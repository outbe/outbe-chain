use crate::transport::tests::*;

#[test]
fn authorized_node_host_client_signs_dev_evidence_in_sgx_no_attest_mode() {
    use outbe_primitives::tee_attestation_v1::{
        AttestationMode, AttestationOperationV1, RegistrationIntentV1,
    };
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
        InitializationState::production_with_challenge_and_attestation(
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
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            serve_connection_with(
                stream,
                &server_keys,
                &offer_key,
                Some(&boot),
                &server_initialization,
            )
            .unwrap();
        }
    });

    let challenge = AuthorizedEnclaveClient::discover_endpoint(&endpoint).unwrap();
    let node_host_path = root.path().join("node-host-noise.key");
    let node_host = NodeHostNoiseKey::create_new(&node_host_path).unwrap();
    let (manifest, node_signature) = signed_initialization_manifest_for_mode(
        &keys,
        challenge.challenge,
        node_host.public(),
        AttestationMode::GramineDirectDev,
    );
    let mut client = AuthorizedEnclaveClient::initialize_endpoint(
        &endpoint,
        &manifest,
        &node_signature,
        &node_host,
    )
    .unwrap();
    let intent = RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::GramineDirectDev,
        policy_hash: B256::repeat_byte(0x55),
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x56),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
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

#[test]
fn public_node_host_client_initializes_and_reconnects_role_neutral_identity() {
    use outbe_tee::{AuthorizedEnclaveClient, NodeHostNoiseKey};

    for seed in [0x71, 0x72] {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("enclave.sock");
        let endpoint = socket.to_str().unwrap().to_string();
        let boot = Arc::new(EnclaveBootConfig::new(
            testnet_chain_word(),
            root.path().to_path_buf(),
            0,
        ));
        let keys = Arc::new(EnclaveKeys::new([seed; 32], Some([seed; 32])).unwrap());
        let initialization = Arc::new(production_dcap_state(boot.clone(), &keys));
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let listener = UnixListener::bind(&socket).unwrap();
        let server_keys = keys.clone();
        let server_boot = boot.clone();
        let server_initialization = initialization.clone();
        let server_offer_key = offer_key.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                serve_connection_with(
                    stream,
                    &server_keys,
                    &server_offer_key,
                    Some(&server_boot),
                    &server_initialization,
                )
                .unwrap();
            }
        });

        let challenge = AuthorizedEnclaveClient::discover_endpoint(&endpoint).unwrap();
        assert_eq!(challenge.recipient_x25519, keys.tribute_offer_public());
        assert_eq!(challenge.attestation_ed25519, keys.attestation_pub());
        assert_eq!(challenge.noise_responder_x25519, keys.noise_public());

        let node_host_path = root.path().join("node-host-noise.key");
        let node_host = NodeHostNoiseKey::create_new(&node_host_path).unwrap();
        let (manifest, node_signature) =
            signed_initialization_manifest(&keys, challenge.challenge, node_host.public());
        let mut initialized = AuthorizedEnclaveClient::initialize_endpoint(
            &endpoint,
            &manifest,
            &node_signature,
            &node_host,
        )
        .unwrap();
        assert!(matches!(
            initialized.request(&EnclaveRequest::GetPublicKeys).unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(initialized);

        drop(node_host);
        let node_host = NodeHostNoiseKey::load(&node_host_path).unwrap();
        let mut reconnected =
            AuthorizedEnclaveClient::connect_endpoint(&endpoint, &manifest, &node_host).unwrap();
        assert!(matches!(
            reconnected.request(&EnclaveRequest::GetPublicKeys).unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(reconnected);
        server.join().unwrap();
        assert_eq!(
            initialization.expected_node_host().unwrap(),
            node_host.public()
        );
    }
}

#[test]
fn preauthenticated_remote_ticket_opens_one_live_noise_session_and_cannot_replay() {
    use outbe_primitives::tee_attestation_v1::{NodeHostAuthorizationWitnessV1, NodeIdV1};
    use outbe_tee::{
        admit_remote_session_v1, AuthorizedEnclaveClient, FinalizedRegistryBindingV1,
        FinalizedRegistryViewV1, NodeHostNoiseKey, RemoteEnclaveClient, RemoteSessionExpectationV1,
    };

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("remote-enclave.sock");
    let endpoint = socket.to_str().unwrap().to_string();
    let boot = Arc::new(EnclaveBootConfig::new(
        testnet_chain_word(),
        root.path().to_path_buf(),
        0,
    ));
    let keys = Arc::new(EnclaveKeys::new([0xA1; 32], Some([0xA1; 32])).unwrap());
    let initialization = Arc::new(production_dcap_state(boot.clone(), &keys));
    let challenge = match initialization.challenge_response(&keys).unwrap() {
        EnclaveResponse::InitializationChallenge { challenge, .. } => challenge,
        response => panic!("unexpected challenge response: {response:?}"),
    };
    let listener = UnixListener::bind(&socket).unwrap();
    let server_keys = keys.clone();
    let server_boot = boot.clone();
    let server_initialization = initialization.clone();
    let server = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let mut connections = Vec::new();
        for _ in 0..4 {
            let (stream, _) = listener.accept().unwrap();
            let keys = server_keys.clone();
            let boot = server_boot.clone();
            let initialization = server_initialization.clone();
            let offer_key = offer_key.clone();
            connections.push(std::thread::spawn(move || {
                serve_connection_with(stream, &keys, &offer_key, Some(&boot), &initialization)
            }));
        }
        connections
            .into_iter()
            .map(|connection| connection.join().unwrap())
            .collect::<Vec<_>>()
    });

    let owner_path = root.path().join("owner-noise.key");
    let owner = NodeHostNoiseKey::create_new(&owner_path).unwrap();
    let (manifest, node_signature) =
        signed_initialization_manifest(&keys, challenge, owner.public());
    let mut owner_client =
        AuthorizedEnclaveClient::initialize_endpoint(&endpoint, &manifest, &node_signature, &owner)
            .unwrap();

    let source_path = root.path().join("source-noise.key");
    let source = NodeHostNoiseKey::create_new(&source_path).unwrap();
    let source_node = NodeIdV1 {
        reth_p2p_public: k256::ecdsa::SigningKey::from_bytes((&[0xB1; 32]).into())
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    let source_witness = NodeHostAuthorizationWitnessV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        attestation_mode: manifest.attestation_mode,
        node_id: source_node.clone(),
        node_host_noise_x25519: source.public(),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(matches!(
        owner_client.request(&EnclaveRequest::AuthorizeRemoteSessionV1 {
            ticket_id: B256::repeat_byte(0xA2),
            initiator_static_x25519: source.public(),
            responder_static_x25519: manifest.noise_responder_x25519,
            deadline: now + 60,
            finalized_block_hash: B256::repeat_byte(0xA3),
        }),
        Err(outbe_tee::TransportError::Handshake(message))
            if message.contains("finalized admission capability")
    ));
    let view = FinalizedRegistryViewV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        block_number: 101,
        block_hash: B256::repeat_byte(0xB3),
        state_root: B256::repeat_byte(0xB4),
        consensus_timestamp: now.saturating_sub(1),
    };
    let source_binding = FinalizedRegistryBindingV1 {
        view,
        node_id_hash: source_node.node_id_hash().unwrap(),
        enclave_id: B256::repeat_byte(0xB5),
        binding_id: B256::repeat_byte(0xB6),
        intent_hash: B256::repeat_byte(0xB7),
        valid_until: now + 600,
        noise_responder_x25519: [0xB8; 32],
        node_host_authorization_hash: source_witness.authorization_hash().unwrap(),
    };
    let target_hash = manifest.node_id.node_id_hash().unwrap();
    let target_binding = FinalizedRegistryBindingV1 {
        view,
        node_id_hash: target_hash,
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0xC1),
        intent_hash: B256::repeat_byte(0xC2),
        valid_until: now + 500,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    };
    let admission = admit_remote_session_v1(
        RemoteSessionExpectationV1 {
            chain_id: manifest.chain_id,
            genesis_hash: manifest.genesis_hash,
            source_node_id_hash: source_binding.node_id_hash,
            target_node_id_hash: target_hash,
        },
        &source_witness,
        source_binding,
        target_binding,
    )
    .unwrap();
    let wrong_ticket = owner_client.authorize_remote_session(&admission).unwrap();
    let mut wrong_stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    wrong_stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    write_frame(
        &mut wrong_stream,
        &outbe_tee::codec::encode_request(&EnclaveRequest::OpenRemoteSessionV1 {
            ticket_id: wrong_ticket.ticket_id(),
        })
        .unwrap(),
    )
    .unwrap();
    let params = NOISE_PARAMS.parse().unwrap();
    let mut wrong_handshake = snow::Builder::new(params)
        .local_private_key(&[0xD1; 32])
        .remote_public_key(&manifest.noise_responder_x25519)
        .build_initiator()
        .unwrap();
    let mut message = [0_u8; 1024];
    let message_len = wrong_handshake.write_message(&[], &mut message).unwrap();
    write_frame(&mut wrong_stream, &message[..message_len]).unwrap();
    assert!(read_frame(&mut wrong_stream).is_err());

    let ticket = owner_client.authorize_remote_session(&admission).unwrap();
    let mut remote = RemoteEnclaveClient::connect_endpoint(&endpoint, &ticket, &source)
        .expect("exact registered source NodeHost must complete Noise IK");
    let remote_keys = remote.public_keys().unwrap();
    assert_eq!(
        remote_keys.noise_static_pub,
        manifest.noise_responder_x25519
    );
    assert_eq!(remote_keys.recipient_x25519_pub, manifest.recipient_x25519);
    assert_eq!(remote_keys.attestation_pub, manifest.attestation_ed25519);
    drop(remote);

    assert!(RemoteEnclaveClient::connect_endpoint(&endpoint, &ticket, &source).is_err());
    drop(owner_client);
    let results = server.join().unwrap();
    assert!(results[0].is_ok());
    assert!(results[1].is_err());
    assert!(results[2].is_ok());
    assert!(results[3].is_err());
}

#[test]
fn node_host_state_initializes_once_and_reconnects_from_datadir() {
    use alloy_primitives::U256;
    use k256::ecdsa::signature::hazmat::PrehashSigner as _;
    use outbe_tee::{connect_or_initialize_node_host_enclave, NodeHostIdentityV1};

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("enclave.sock");
    let endpoint = socket.to_str().unwrap().to_string();
    let chain_id = outbe_primitives::chain::TESTNET_CHAIN_ID;
    let chain_id_word = U256::from(chain_id).to_be_bytes();
    let genesis_hash = B256::repeat_byte(0x11);
    let boot = Arc::new(EnclaveBootConfig::new(
        chain_id_word,
        root.path().join("enclave-state"),
        0,
    ));
    std::fs::create_dir(&boot.tee_dir).unwrap();
    let keys = Arc::new(EnclaveKeys::new([0x74; 32], Some([0x74; 32])).unwrap());
    let initialization = Arc::new(production_dcap_state(boot.clone(), &keys));
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let listener = UnixListener::bind(&socket).unwrap();
    let server_keys = keys.clone();
    let server_initialization = initialization.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let (stream, _) = listener.accept().unwrap();
            serve_connection_with(
                stream,
                &server_keys,
                &offer_key,
                Some(&boot),
                &server_initialization,
            )
            .unwrap();
        }
    });

    let signing = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let identity = NodeHostIdentityV1 {
        network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
            chain_id: U256::from(chain_id).to_be_bytes(),
            genesis_hash,
            attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
        },
        reth_p2p_public: signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    let sign = |hash: B256| {
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
            .sign_prehash(hash.as_slice())
            .map_err(|error| error.to_string())?;
        let mut bytes = [0_u8; 65];
        bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
        bytes[64] = recovery.to_byte();
        Ok(bytes)
    };
    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();

    let mut initialized =
        connect_or_initialize_node_host_enclave(&endpoint, &node_data_dir, identity, sign).unwrap();
    assert!(matches!(
        initialized.request(&EnclaveRequest::GetPublicKeys).unwrap(),
        EnclaveResponse::PublicKeys { .. }
    ));
    drop(initialized);

    let mut reconnected =
        connect_or_initialize_node_host_enclave(&endpoint, &node_data_dir, identity, sign).unwrap();
    assert!(matches!(
        reconnected.request(&EnclaveRequest::GetPublicKeys).unwrap(),
        EnclaveResponse::PublicKeys { .. }
    ));
    drop(reconnected);
    server.join().unwrap();

    let state = node_data_dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    assert!(state
        .join(outbe_tee::node_host::NODE_HOST_NOISE_KEY_V1)
        .is_file());
    assert!(state
        .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1)
        .is_file());
}

#[test]
fn node_host_replacement_candidate_keeps_the_committed_enclave_active() {
    use alloy_primitives::U256;
    use k256::ecdsa::signature::hazmat::PrehashSigner as _;
    use outbe_primitives::tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapCollateralComponentV1,
        DcapCollateralKind, DcapEvidenceV1, EnclaveInitializationManifestV1, RegistrationIntentV1,
    };
    use outbe_tee::{
        connect_or_initialize_node_host_enclave, persist_replacement_candidate_submission,
        prepare_node_host_enclave_replacement_candidate, NodeHostIdentityV1,
    };

    let root = tempfile::tempdir().unwrap();
    let socket_a = root.path().join("enclave-a.sock");
    let socket_b = root.path().join("enclave-b.sock");
    let endpoint_a = socket_a.to_str().unwrap().to_string();
    let endpoint_b = socket_b.to_str().unwrap().to_string();
    let chain_id = outbe_primitives::chain::TESTNET_CHAIN_ID;
    let chain_id_word = U256::from(chain_id).to_be_bytes();
    let genesis_hash = B256::repeat_byte(0x13);

    let boot_a = Arc::new(EnclaveBootConfig::new(
        chain_id_word,
        root.path().join("enclave-a-state"),
        0,
    ));
    let boot_b = Arc::new(EnclaveBootConfig::new(
        chain_id_word,
        root.path().join("enclave-b-state"),
        0,
    ));
    std::fs::create_dir(&boot_a.tee_dir).unwrap();
    std::fs::create_dir(&boot_b.tee_dir).unwrap();
    let keys_a = Arc::new(EnclaveKeys::new([0x76; 32], Some([0x76; 32])).unwrap());
    let keys_b = Arc::new(EnclaveKeys::new([0x77; 32], Some([0x77; 32])).unwrap());
    let initialization_a = Arc::new(production_dcap_state(boot_a.clone(), &keys_a));
    let initialization_b = Arc::new(production_dcap_state(boot_b.clone(), &keys_b));

    let listener_a = UnixListener::bind(&socket_a).unwrap();
    let server_keys_a = keys_a.clone();
    let server_a = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        for _ in 0..3 {
            let (stream, _) = listener_a.accept().unwrap();
            serve_connection_with(
                stream,
                &server_keys_a,
                &offer_key,
                Some(&boot_a),
                &initialization_a,
            )
            .unwrap();
        }
    });
    let listener_b = UnixListener::bind(&socket_b).unwrap();
    let server_keys_b = keys_b.clone();
    let server_b = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        for _ in 0..3 {
            let (stream, _) = listener_b.accept().unwrap();
            serve_connection_with(
                stream,
                &server_keys_b,
                &offer_key,
                Some(&boot_b),
                &initialization_b,
            )
            .unwrap();
        }
    });

    let signing = k256::ecdsa::SigningKey::from_bytes((&[0x63; 32]).into()).unwrap();
    let identity = NodeHostIdentityV1 {
        network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
            chain_id: U256::from(chain_id).to_be_bytes(),
            genesis_hash,
            attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
        },
        reth_p2p_public: signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    let sign = |hash: B256| {
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
            .sign_prehash(hash.as_slice())
            .map_err(|error| error.to_string())?;
        let mut bytes = [0_u8; 65];
        bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
        bytes[64] = recovery.to_byte();
        Ok(bytes)
    };
    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();

    drop(
        connect_or_initialize_node_host_enclave(&endpoint_a, &node_data_dir, identity, sign)
            .unwrap(),
    );
    let candidate = prepare_node_host_enclave_replacement_candidate(
        &endpoint_b,
        &node_data_dir,
        identity,
        sign,
    )
    .unwrap();
    let active_bytes = std::fs::read(
        node_data_dir
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1),
    )
    .unwrap();
    let active = EnclaveInitializationManifestV1::decode_canonical(&active_bytes).unwrap();
    assert_ne!(
        active.enclave_id().unwrap(),
        candidate.manifest().enclave_id().unwrap()
    );
    assert_eq!(
        active.node_host_authorization_hash().unwrap(),
        candidate.manifest().node_host_authorization_hash().unwrap()
    );
    let candidate_manifest = candidate.manifest().clone();
    let candidate_path = node_data_dir
        .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
        .join(outbe_tee::node_host::NODE_HOST_REPLACEMENT_CANDIDATE_V1);
    let candidate_bytes = std::fs::read(&candidate_path).unwrap();
    drop(candidate);
    let resumed = prepare_node_host_enclave_replacement_candidate(
        &endpoint_b,
        &node_data_dir,
        identity,
        sign,
    )
    .unwrap();
    assert_eq!(resumed.manifest(), &candidate_manifest);
    assert_eq!(std::fs::read(&candidate_path).unwrap(), candidate_bytes);
    drop(resumed);

    let intent = RegistrationIntentV1 {
        chain_id: candidate_manifest.chain_id,
        genesis_hash: candidate_manifest.genesis_hash,
        operation: AttestationOperationV1::ReplaceEnclaveBinding,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: B256::repeat_byte(0x21),
        node_id: candidate_manifest.node_id.clone(),
        enclave_id: candidate_manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x45),
        binding_version: 2,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 20_000,
        recipient_x25519: candidate_manifest.recipient_x25519,
        attestation_ed25519: candidate_manifest.attestation_ed25519,
        noise_responder_x25519: candidate_manifest.noise_responder_x25519,
        node_host_authorization_hash: candidate_manifest.node_host_authorization_hash().unwrap(),
    };
    candidate_manifest.validate_intent_binding(&intent).unwrap();
    let intent_hash = intent.intent_hash().unwrap();
    let node_signature = sign(intent_hash).unwrap();
    let enclave_signature = keys_b.sign_attestation(intent_hash.as_slice());
    let components = (1_u8..=8)
        .map(|kind| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(kind).unwrap(),
            bytes: vec![kind],
        })
        .collect();
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: vec![0x51],
        components,
        transition_key_ready_proof: None,
    });
    let submission = persist_replacement_candidate_submission(
        &node_data_dir,
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();
    assert_eq!(
        persist_replacement_candidate_submission(
            &node_data_dir,
            &evidence,
            &node_signature,
            &enclave_signature,
        )
        .unwrap(),
        submission
    );
    let mut conflicting = evidence;
    let AttestationEvidenceV1::Dcap(conflicting_dcap) = &mut conflicting else {
        unreachable!();
    };
    conflicting_dcap.quote[0] ^= 1;
    assert!(persist_replacement_candidate_submission(
        &node_data_dir,
        &conflicting,
        &node_signature,
        &enclave_signature,
    )
    .unwrap_err()
    .to_string()
    .contains("conflicts with the durable replacement submission"));

    let mut active_client =
        connect_or_initialize_node_host_enclave(&endpoint_a, &node_data_dir, identity, sign)
            .unwrap();
    assert!(matches!(
        active_client
            .request(&EnclaveRequest::GetPublicKeys)
            .unwrap(),
        EnclaveResponse::PublicKeys { .. }
    ));
    drop(active_client);
    server_a.join().unwrap();
    server_b.join().unwrap();
}

#[test]
fn replacement_candidate_resumes_after_crash_between_stage_and_initialize() {
    use alloy_primitives::U256;
    use k256::ecdsa::signature::hazmat::PrehashSigner as _;
    use outbe_tee::{
        connect_or_initialize_node_host_enclave, prepare_node_host_enclave_replacement_candidate,
        NodeHostIdentityV1,
    };

    let root = tempfile::tempdir().unwrap();
    let active_socket = root.path().join("active.sock");
    let candidate_socket = root.path().join("candidate.sock");
    let active_endpoint = active_socket.to_str().unwrap().to_string();
    let candidate_endpoint = candidate_socket.to_str().unwrap().to_string();
    let chain_id = outbe_primitives::chain::TESTNET_CHAIN_ID;
    let chain_id_word = U256::from(chain_id).to_be_bytes();
    let genesis_hash = B256::repeat_byte(0x15);
    let active_boot = Arc::new(EnclaveBootConfig::new(
        chain_id_word,
        root.path().join("active-state"),
        0,
    ));
    let candidate_boot = Arc::new(EnclaveBootConfig::new(
        chain_id_word,
        root.path().join("candidate-state"),
        0,
    ));
    std::fs::create_dir(&active_boot.tee_dir).unwrap();
    std::fs::create_dir(&candidate_boot.tee_dir).unwrap();
    let active_keys = Arc::new(EnclaveKeys::new([0x79; 32], Some([0x79; 32])).unwrap());
    let candidate_keys = Arc::new(EnclaveKeys::new([0x7a; 32], Some([0x7a; 32])).unwrap());
    let active_initialization = Arc::new(production_dcap_state(active_boot.clone(), &active_keys));
    let candidate_initialization = Arc::new(production_dcap_state(
        candidate_boot.clone(),
        &candidate_keys,
    ));

    let active_listener = UnixListener::bind(&active_socket).unwrap();
    let active_server_keys = active_keys.clone();
    let active_server = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        for _ in 0..2 {
            let (stream, _) = active_listener.accept().unwrap();
            serve_connection_with(
                stream,
                &active_server_keys,
                &offer_key,
                Some(&active_boot),
                &active_initialization,
            )
            .unwrap();
        }
    });

    let signing = k256::ecdsa::SigningKey::from_bytes((&[0x64; 32]).into()).unwrap();
    let identity = NodeHostIdentityV1 {
        network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
            chain_id: U256::from(chain_id).to_be_bytes(),
            genesis_hash,
            attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
        },
        reth_p2p_public: signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    let sign = |hash: B256| {
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
            .sign_prehash(hash.as_slice())
            .map_err(|error| error.to_string())?;
        let mut bytes = [0_u8; 65];
        bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
        bytes[64] = recovery.to_byte();
        Ok(bytes)
    };
    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();
    drop(
        connect_or_initialize_node_host_enclave(&active_endpoint, &node_data_dir, identity, sign)
            .unwrap(),
    );
    active_server.join().unwrap();

    let first_listener = UnixListener::bind(&candidate_socket).unwrap();
    let first_server_keys = candidate_keys.clone();
    let first_boot = candidate_boot.clone();
    let first_initialization = candidate_initialization.clone();
    let first_server = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let (stream, _) = first_listener.accept().unwrap();
        serve_connection_with(
            stream,
            &first_server_keys,
            &offer_key,
            Some(&first_boot),
            &first_initialization,
        )
        .unwrap();
    });
    assert!(prepare_node_host_enclave_replacement_candidate(
        &candidate_endpoint,
        &node_data_dir,
        identity,
        sign,
    )
    .is_err());
    first_server.join().unwrap();
    std::fs::remove_file(&candidate_socket).unwrap();
    drop(candidate_initialization);
    let restarted_candidate_initialization = Arc::new(production_dcap_state(
        candidate_boot.clone(),
        &candidate_keys,
    ));

    let retry_listener = UnixListener::bind(&candidate_socket).unwrap();
    retry_listener.set_nonblocking(true).unwrap();
    let retry_server_keys = candidate_keys.clone();
    let retry_server = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut accepted = 0;
        while accepted < 3 && std::time::Instant::now() < deadline {
            match retry_listener.accept() {
                Ok((stream, _)) => {
                    // macOS propagates O_NONBLOCK from the listener to the
                    // accepted socket. The enclave protocol itself is
                    // intentionally blocking, so restore that contract.
                    stream.set_nonblocking(false).unwrap();
                    let result = serve_connection_with(
                        stream,
                        &retry_server_keys,
                        &offer_key,
                        Some(&candidate_boot),
                        &restarted_candidate_initialization,
                    );
                    if let Err(error) = result {
                        assert!(
                            error.to_string().contains("enclave is not initialized")
                                || error.to_string().contains("challenge mismatch"),
                            "unexpected candidate retry error: {error}"
                        );
                    }
                    accepted += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => panic!("candidate retry listener failed: {error}"),
            }
        }
        accepted
    });
    let retry = prepare_node_host_enclave_replacement_candidate(
        &candidate_endpoint,
        &node_data_dir,
        identity,
        sign,
    );
    match retry {
        Ok(candidate) => drop(candidate),
        Err(error) => {
            let accepted = retry_server.join().unwrap();
            panic!("candidate retry failed after {accepted} connections: {error}");
        }
    }
    assert_eq!(retry_server.join().unwrap(), 3);
}

#[test]
fn node_host_replacement_preserves_exact_reth_p2p_identity() {
    use alloy_primitives::U256;
    use k256::ecdsa::signature::hazmat::PrehashSigner as _;
    use outbe_primitives::tee_attestation_v1::{EnclaveInitializationManifestV1, NodeIdV1};
    use outbe_tee::{
        connect_or_initialize_node_host_enclave, prepare_node_host_enclave_replacement_candidate,
        NodeHostIdentityV1,
    };

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("enclave.sock");
    let endpoint = socket.to_str().unwrap().to_string();
    let candidate_socket = root.path().join("candidate-enclave.sock");
    let candidate_endpoint = candidate_socket.to_str().unwrap().to_string();
    let chain_id = outbe_primitives::chain::TESTNET_CHAIN_ID;
    let chain_id_word = U256::from(chain_id).to_be_bytes();
    let genesis_hash = B256::repeat_byte(0x12);
    let boot = Arc::new(EnclaveBootConfig::new(
        chain_id_word,
        root.path().join("enclave-state"),
        0,
    ));
    std::fs::create_dir(&boot.tee_dir).unwrap();
    let keys = Arc::new(EnclaveKeys::new([0x75; 32], Some([0x75; 32])).unwrap());
    let initialization = Arc::new(production_dcap_state(boot.clone(), &keys));
    let candidate_boot = Arc::new(EnclaveBootConfig::new(
        chain_id_word,
        root.path().join("candidate-enclave-state"),
        0,
    ));
    std::fs::create_dir(&candidate_boot.tee_dir).unwrap();
    let candidate_keys = Arc::new(EnclaveKeys::new([0x78; 32], Some([0x78; 32])).unwrap());
    let candidate_initialization = Arc::new(production_dcap_state(
        candidate_boot.clone(),
        &candidate_keys,
    ));
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let listener = UnixListener::bind(&socket).unwrap();
    let server_keys = keys.clone();
    let server_initialization = initialization.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let (stream, _) = listener.accept().unwrap();
            serve_connection_with(
                stream,
                &server_keys,
                &offer_key,
                Some(&boot),
                &server_initialization,
            )
            .unwrap();
        }
    });
    let candidate_offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let candidate_listener = UnixListener::bind(&candidate_socket).unwrap();
    let candidate_server_keys = candidate_keys.clone();
    let candidate_server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = candidate_listener.accept().unwrap();
            serve_connection_with(
                stream,
                &candidate_server_keys,
                &candidate_offer_key,
                Some(&candidate_boot),
                &candidate_initialization,
            )
            .unwrap();
        }
    });

    let signing = k256::ecdsa::SigningKey::from_bytes((&[0x62; 32]).into()).unwrap();
    let reth_p2p_public: [u8; 33] = signing
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let identity = NodeHostIdentityV1 {
        network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
            chain_id: U256::from(chain_id).to_be_bytes(),
            genesis_hash,
            attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
        },
        reth_p2p_public,
    };
    let sign = |hash: B256| {
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
            .sign_prehash(hash.as_slice())
            .map_err(|error| error.to_string())?;
        let mut bytes = [0_u8; 65];
        bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
        bytes[64] = recovery.to_byte();
        Ok(bytes)
    };
    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();

    let mut initialized =
        connect_or_initialize_node_host_enclave(&endpoint, &node_data_dir, identity, sign).unwrap();
    assert!(matches!(
        initialized.request(&EnclaveRequest::GetPublicKeys).unwrap(),
        EnclaveResponse::PublicKeys { .. }
    ));
    drop(initialized);

    let mut reconnected =
        connect_or_initialize_node_host_enclave(&endpoint, &node_data_dir, identity, sign).unwrap();
    assert!(matches!(
        reconnected.request(&EnclaveRequest::GetPublicKeys).unwrap(),
        EnclaveResponse::PublicKeys { .. }
    ));
    drop(reconnected);
    let candidate = prepare_node_host_enclave_replacement_candidate(
        &candidate_endpoint,
        &node_data_dir,
        identity,
        sign,
    )
    .unwrap();
    assert_eq!(candidate.manifest().node_id, NodeIdV1 { reth_p2p_public });
    let candidate_manifest = candidate.manifest().clone();
    drop(candidate);
    server.join().unwrap();
    candidate_server.join().unwrap();

    let manifest_bytes = std::fs::read(
        node_data_dir
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1),
    )
    .unwrap();
    let manifest = EnclaveInitializationManifestV1::decode_canonical(&manifest_bytes).unwrap();
    assert_eq!(manifest.node_id, NodeIdV1 { reth_p2p_public });
    assert_ne!(
        manifest.enclave_id().unwrap(),
        candidate_manifest.enclave_id().unwrap()
    );
    assert_eq!(
        manifest.node_host_authorization_hash().unwrap(),
        candidate_manifest.node_host_authorization_hash().unwrap()
    );
}
