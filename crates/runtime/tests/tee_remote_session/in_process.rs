//! Finalized direct admission must bind the actual RAM identity, never an IPC ticket.
use super::*;
use outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1;
use outbe_tee::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
    NodeHostIdentityV1,
};

#[test]
fn direct_admission_requires_exact_resident_manifest_host_and_noise_at_finality() {
    let keys = ResidentTeeKeys::from_seed([8; 32]).unwrap();
    let genesis_hash = B256::repeat_byte(0x31);
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: chain_id_word(CHAIN_ID),
        genesis_hash,
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id: full_node(0x41),
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let context = InProcessTeeContext::new(
        keys,
        NodeHostIdentityV1 {
            network_binding: manifest.network_binding(),
            reth_p2p_public: manifest.node_id.reth_p2p_public,
        },
        manifest.clone(),
        TrustedNetworkDescriptorV1 {
            network_binding: manifest.network_binding(),
            genesis_consensus_keys: vec![],
        },
        ProofSourceV1::NoHardware,
    )
    .unwrap();
    let source = NodeHostAuthorizationWitnessV1 {
        chain_id: manifest.chain_id,
        genesis_hash,
        attestation_mode: manifest.attestation_mode,
        node_id: full_node(0x31),
        node_host_noise_x25519: [0x41; 32],
    };
    for defect in [
        None,
        Some("manifest"),
        Some("host"),
        Some("noise"),
        Some("expiry"),
        Some("chain"),
    ] {
        let mut target = SeedBinding {
            enclave_id: manifest.enclave_id().unwrap(),
            binding_id: B256::repeat_byte(0x53),
            intent_hash: B256::repeat_byte(0x54),
            valid_until: CONSENSUS_TIMESTAMP + 400,
            noise_responder_x25519: context.noise_public(),
            node_host_authorization_hash: context
                .authorization_witness()
                .unwrap()
                .authorization_hash()
                .unwrap(),
        };
        match defect {
            Some("manifest") => target.enclave_id = B256::repeat_byte(9),
            Some("host") => target.node_host_authorization_hash = B256::repeat_byte(9),
            Some("noise") => target.noise_responder_x25519 = [9; 32],
            Some("expiry") => target.valid_until = CONSENSUS_TIMESTAMP,
            _ => {}
        }
        let mut seed = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
        StorageHandle::enter(&mut seed, |storage| {
            let registry = TeeRegistry::new(storage);
            seed_binding(
                &registry,
                &source.node_id,
                SeedBinding {
                    enclave_id: B256::repeat_byte(0x42),
                    binding_id: B256::repeat_byte(0x43),
                    intent_hash: B256::repeat_byte(0x44),
                    valid_until: CONSENSUS_TIMESTAMP + 500,
                    noise_responder_x25519: [0x45; 32],
                    node_host_authorization_hash: source.authorization_hash().unwrap(),
                },
            );
            seed_binding(&registry, &manifest.node_id, target);
            let node_hash = manifest.node_id.node_id_hash().unwrap();
            registry
                .v1_node_attestation_ed25519
                .write(&node_hash, B256::from(manifest.attestation_ed25519))
                .unwrap();
            registry
                .v1_node_recipient_x25519
                .write(&node_hash, B256::from(manifest.recipient_x25519))
                .unwrap();
        });
        let provider = MockEthProvider::<OutbePrimitives>::new().with_chain_spec(
            ChainSpecBuilder::mainnet()
                .build()
                .map_header(OutbeHeader::new),
        );
        provider.add_account(
            outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
            ExtendedAccount::new(0, U256::ZERO)
                .with_bytecode(Bytes::from_static(&[0xef]))
                .extend_storage(
                    seed.storage
                        .into_iter()
                        .map(|((_, slot), value)| (B256::from(slot.to_be_bytes::<32>()), value)),
                ),
        );
        let header = OutbeHeader::new(Header {
            number: 90,
            timestamp: CONSENSUS_TIMESTAMP,
            state_root: B256::repeat_byte(0x34),
            ..Default::default()
        });
        let hash = header.hash_slow();
        provider.add_header(hash, header);
        let finalized = FinalizedMockProvider::new(provider, BlockNumHash::new(90, hash));
        let network = if defect == Some("chain") {
            B256::repeat_byte(7)
        } else {
            genesis_hash
        };
        let result = authorize_local_finalized_remote_session_v1(
            &finalized,
            LocalRemoteSessionTargetV1::InProcess(&context),
            RegistryChainIdentity {
                chain_id: CHAIN_ID,
                genesis_hash: network,
            },
            RemoteSessionRequest {
                expected: RemoteSessionExpectationV1 {
                    chain_id: manifest.chain_id,
                    genesis_hash: network,
                    source_node_id_hash: source.node_id.node_id_hash().unwrap(),
                    target_node_id_hash: manifest.node_id.node_id_hash().unwrap(),
                },
                source_witness: &source,
                target_node: &manifest.node_id,
            },
            &outbe_runtime::OutbeRuntime::new(),
        );
        if defect.is_some() {
            assert!(result.is_err(), "must reject {defect:?}");
        } else {
            let LocalRemoteSessionAuthorizationV1::InProcess(admission) = result.unwrap() else {
                panic!("must not produce an IPC ticket")
            };
            assert_eq!(admission.finalized_view().block_hash, hash);
            assert_eq!(admission.responder_static_x25519(), context.noise_public());
            assert_eq!(
                admission.initiator_static_x25519(),
                source.node_host_noise_x25519
            );
            assert_eq!(admission.deadline(), CONSENSUS_TIMESTAMP + 400);
        }
    }
}
