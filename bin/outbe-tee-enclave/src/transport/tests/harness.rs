use crate::transport::tests::*;

pub(in crate::transport::tests) fn spawn_production_connection(
    keys: Arc<EnclaveKeys>,
    boot: Arc<EnclaveBootConfig>,
    offer_key: SharedTributeOfferKey,
    initialization: Arc<InitializationState>,
) -> (
    std::os::unix::net::UnixStream,
    std::thread::JoinHandle<Result<(), TransportError>>,
) {
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    let handle = std::thread::spawn(move || {
        serve_connection_with(server, &keys, &offer_key, Some(&boot), &initialization)
    });
    (client, handle)
}

/// One in-process enclave: its key material, resident DKG store, and the
/// shared DKG-derived offer key slot.
pub(in crate::transport::tests) struct Enclave {
    _root: tempfile::TempDir,
    pub(in crate::transport::tests) boot: Arc<EnclaveBootConfig>,
    pub(in crate::transport::tests) initialization: InitializationState,
    pub(in crate::transport::tests) keys: EnclaveKeys,
    pub(in crate::transport::tests) dkg: DkgSessionStore,
    pub(in crate::transport::tests) offer_key: SharedTributeOfferKey,
    pub(in crate::transport::tests) chain_id: B256,
}

impl Enclave {
    pub(in crate::transport::tests) fn new(seed: u8) -> Self {
        let root = tempfile::tempdir().unwrap();
        let boot = Arc::new(EnclaveBootConfig::new(
            testnet_chain_word(),
            root.path().to_path_buf(),
            1,
        ));
        let keys = EnclaveKeys::new([seed; 32], Some([seed; 32])).expect("keys");
        let challenge = [seed.wrapping_add(0x40); 32];
        let (manifest, node_signature) =
            signed_initialization_manifest(&keys, challenge, [0x43; 32]);
        let initialization = InitializationState::production_with_trusted_network_descriptor(
            boot.clone(),
            &keys,
            challenge,
            outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1 {
                network_binding: manifest.network_binding(),
                genesis_consensus_keys: vec![[0x61; 48]],
            },
        )
        .unwrap();
        let pending = initialization
            .prepare(
                &manifest.encode_canonical().unwrap(),
                &node_signature,
                &keys,
            )
            .unwrap();
        initialization.commit(pending, &keys).unwrap();
        Self {
            _root: root,
            boot,
            initialization,
            keys,
            dkg: DkgSessionStore::new(),
            offer_key: Arc::new(OnceLock::new()),
            chain_id: B256::from(testnet_chain_word()),
        }
    }

    pub(in crate::transport::tests) fn call(&mut self, req: EnclaveRequest) -> EnclaveResponse {
        dispatch_with_initialization(
            req,
            &self.keys,
            &mut self.dkg,
            &self.offer_key,
            self.chain_id,
            DispatchInitializationContext {
                boot: Some(&self.boot),
                initialization: Some(&self.initialization),
                quote_generator: crate::gramine::dcap_quote,
            },
        )
    }

    pub(in crate::transport::tests) fn bls_public(&mut self) -> Vec<u8> {
        match self.call(EnclaveRequest::GetPublicKeys) {
            EnclaveResponse::PublicKeys { tee_bls_pub, .. } => tee_bls_pub,
            other => panic!("unexpected GetPublicKeys response: {other:?}"),
        }
    }

    pub(in crate::transport::tests) fn identity(
        &mut self,
        ceremony_id: B256,
        participant_bls: Vec<Vec<u8>>,
    ) -> outbe_tee::protocol::ParticipantAnnounce {
        match self.call(EnclaveRequest::DkgParticipantAnnounceV1 {
            ceremony_id,
            round: 0,
            participant_bls,
        }) {
            EnclaveResponse::DkgParticipantAnnounceV1 { participant } => participant,
            other => panic!("unexpected DKG announcement response: {other:?}"),
        }
    }
}

pub(in crate::transport::tests) fn persist_test_offer_key(
    cfg: &EnclaveBootConfig,
    binding: NetworkBindingV1,
    offer_key: &SharedTributeOfferKey,
) -> Result<(), String> {
    persist_offer_key_required(cfg, binding, offer_key.get().expect("resident test key"))
}

/// An enclave with a resident group key installed, so `DeriveAccountKeys` can
/// exercise the ready-state capability in isolation.
pub(in crate::transport::tests) fn resident_enclave(seed: u8) -> Enclave {
    let group_sig = vec![0x5a_u8; 96];
    let e = Enclave::new(seed);
    let (secret, public) =
        crate::crypto::derive_tribute_offer_secret_from_group_sig(&group_sig, e.chain_id, 0)
            .expect("derive");
    e.offer_key
        .set(DerivedTributeOfferKey::from_parts(
            secret,
            public,
            Zeroizing::new(group_sig),
        ))
        .ok()
        .expect("install");
    e
}
