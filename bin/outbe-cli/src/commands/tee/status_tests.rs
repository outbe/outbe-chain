use super::*;
use crate::rpc::mock::{call_map, MockRpc};
use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolValue;
use outbe_primitives::{
    chain::DEVNET_CHAIN_ID,
    tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1},
    tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::NodeEnclaveBindingV1View,
};

fn status_args() -> StatusArgs {
    StatusArgs {
        node_data_dir: None,
        warning_blocks: 600,
        critical_blocks: 120,
    }
}

fn status_rpc() -> MockRpc {
    let policy = initial_tee_policy_v1(
        InitialTeeProfileV1::NoAttestationDev,
        DEVNET_CHAIN_ID,
        B256::repeat_byte(2),
    )
    .unwrap();
    let calls = [
        (
            ITeeRegistry::activePolicyV1Call::SELECTOR,
            Bytes::from(policy.encode_canonical().unwrap()).abi_encode(),
        ),
        (
            ITeeRegistry::validatorEnclaveBindingCall::SELECTOR,
            NodeEnclaveBindingV1View::default().abi_encode(),
        ),
        (
            ITeeRegistry::tributeOfferPublicKeyCall::SELECTOR,
            U256::from(7).abi_encode(),
        ),
        (
            ITeeRegistry::enclaveUpgradeV1Call::SELECTOR,
            (
                U256::from(9),
                200_u64,
                B256::repeat_byte(3),
                B256::repeat_byte(4),
                B256::repeat_byte(5),
            )
                .abi_encode_params(),
        ),
        (
            ITeeRegistry::stagedSuccessorPolicyV1Call::SELECTOR,
            (
                true,
                U256::from(9),
                Bytes::from(policy.encode_canonical().unwrap()),
            )
                .abi_encode_params(),
        ),
    ];
    MockRpc {
        chain_id: Ok(DEVNET_CHAIN_ID),
        finalized_block: Ok(json!({
            "number": "0x78", "timestamp": "0x3e8", "hash": B256::repeat_byte(1), "stateRoot": B256::repeat_byte(6),
        })),
        tee_renewal_schedule: Ok(TeeRenewalScheduleV1 {
            finalized_height: 120,
            finalized_hash: B256::repeat_byte(1),
            finalized_timestamp: 1_000,
            epoch_number: 2,
            epoch_start_height: 100,
            epoch_length_blocks: 100,
            next_freeze_height: 180,
            planned_activation_height: 200,
            dkg_prepare_window_blocks: 20,
            minimum_block_time_millis: 2_000,
        }),
        eth_call_map: Some(call_map(
            calls
                .into_iter()
                .map(|(selector, response)| ((TEE_REGISTRY_ADDR, selector), response))
                .collect(),
        )),
        consensus_status: Ok(json!({"enclave": {"state": "ready", "ready": true}})),
        ..Default::default()
    }
}

#[tokio::test]
async fn status_reads_registry_policy_upgrade_and_local_health_without_signing() {
    let status = read_status(&status_rpc(), &status_args()).await.unwrap();
    assert_eq!(status["schedule"]["finalizedHeight"], 120);
    assert_eq!(status["activePolicy"]["attestationMode"], 2);
    assert_eq!(
        status["networkEncryptionPublicKey"],
        json!(B256::from(U256::from(7).to_be_bytes::<32>()))
    );
    assert!(status["binding"].is_null());
    assert!(status["lease"].is_null());
    assert_eq!(status["upgrade"]["proposalId"], json!(U256::from(9)));
    assert_eq!(status["upgrade"]["activationHeight"], 200);
    assert_eq!(status["stagedSuccessor"]["exists"], true);
    assert_eq!(status["enclave"], json!({"state": "ready", "ready": true}));
}

#[tokio::test]
async fn status_rejects_invalid_margins_missing_health_and_absent_manifest_without_writes() {
    let mut args = status_args();
    args.critical_blocks = args.warning_blocks + 1;
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::CriticalMarginExceedsWarning)
    ));

    let mut rpc = status_rpc();
    rpc.consensus_status = Ok(json!({}));
    let error = read_status(&rpc, &status_args()).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::MissingEnclaveHealth)
    ));

    let scratch = tempfile::tempdir().unwrap();
    let absent = scratch.path().join("absent-node");
    args = status_args();
    args.node_data_dir = Some(absent.clone());
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::ReadPublicManifest(_))
    ));
    assert!(!absent.exists());
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn status_public_manifest_does_not_load_keys_lock_or_reconcile_journals() {
    use outbe_primitives::tee_attestation_v1::NodeIdV1;
    use outbe_tee::node_host::{NODE_HOST_DIRECTORY_V1, NODE_HOST_MANIFEST_V1};
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join(NODE_HOST_DIRECTORY_V1);
    std::fs::create_dir(&root).unwrap();
    let signer = k256::ecdsa::SigningKey::from_bytes((&[1; 32]).into()).unwrap();
    let mut manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(DEVNET_CHAIN_ID).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id: NodeIdV1 {
            reth_p2p_public: signer
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: [5; 32],
        attestation_ed25519: [6; 32],
        noise_responder_x25519: [7; 32],
    };
    let path = root.join(NODE_HOST_MANIFEST_V1);
    let original = manifest.encode_canonical().unwrap();
    std::fs::write(&path, &original).unwrap();
    let args = StatusArgs {
        node_data_dir: Some(scratch.path().to_path_buf()),
        ..status_args()
    };
    let mut rpc = status_rpc();
    rpc.eth_call_overrides.insert(
        (
            TEE_REGISTRY_ADDR,
            NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public).binding_call(),
        ),
        NodeEnclaveBindingV1View {
            exists: true,
            validUntil: 2_000,
            ..Default::default()
        }
        .abi_encode(),
    );
    let status = read_status(&rpc, &args).await.unwrap();
    assert_eq!(status["binding"]["validUntil"], 2_000);
    assert_eq!(status["lease"]["safeForNextFreeze"], true);
    assert!(status["lease"]["collateralValidUntil"].is_null());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    manifest.genesis_hash = B256::repeat_byte(8);
    std::fs::write(&path, manifest.encode_canonical().unwrap()).unwrap();
    let error = read_status(&rpc, &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::ManifestChainIdentityMismatch)
    ));
}
