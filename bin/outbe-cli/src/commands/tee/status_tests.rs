use super::*;
use crate::rpc::mock::{call_map, MockRpc};
use alloy_primitives::{Address, Bytes, B256, U256};
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
        node_id: None,
        validator: None,
        reth_key_backend: "plaintext".to_owned(),
        reth_passphrase: None,
        warning_blocks: 600,
        critical_blocks: 120,
    }
}

/// Expected NodeHost id from the scalar itself, not from the custody file.
fn node_id_for_seed(seed: &[u8; 32]) -> [u8; 33] {
    k256::ecdsa::SigningKey::from_slice(seed)
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap()
}

/// Provision `<datadir>/discovery-secret` with the production custody writer.
fn install_discovery_secret(
    dir: &std::path::Path,
    seed: &[u8; 32],
    backend: &outbe_simplex::bls::KeyBackend,
) -> [u8; 33] {
    let node_id = node_id_for_seed(seed);
    outbe_simplex::bls::save_secp256k1_secret(&dir.join("discovery-secret"), seed, backend)
        .unwrap();
    node_id
}

fn write_owner_file(path: &std::path::Path, bytes: &[u8]) {
    write_exact_mode(path, bytes, 0o600);
}

fn write_exact_mode(path: &std::path::Path, bytes: &[u8], mode: u32) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .unwrap();
}

fn bind_node(rpc: &mut MockRpc, selector: NodeBindingSelectorV1, valid_until: u64) {
    rpc.eth_call_overrides.insert(
        (TEE_REGISTRY_ADDR, selector.binding_call()),
        NodeEnclaveBindingV1View {
            exists: true,
            validUntil: valid_until,
            ..Default::default()
        }
        .abi_encode(),
    );
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
        Some(TeeStatusError::ReadDiscoverySecret(_))
    ));
    assert!(!absent.exists());
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);

    let datadir = scratch.path().join("node");
    std::fs::create_dir(&datadir).unwrap();
    args.node_data_dir = Some(datadir.clone());
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::ReadDiscoverySecret(_))
    ));
    assert_eq!(std::fs::read_dir(&datadir).unwrap().count(), 0);

    let secret_path = datadir.join("discovery-secret");
    write_owner_file(&secret_path, b"zz");
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::InvalidDiscoverySecret(_))
    ));

    outbe_simplex::bls::save_secp256k1_secret(
        &secret_path,
        &[0; 32],
        &outbe_simplex::bls::KeyBackend::Plaintext,
    )
    .unwrap();
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::InvalidDiscoverySecret(_))
    ));
    write_owner_file(&secret_path, &b"g".repeat(64));
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::InvalidDiscoverySecret(_))
    ));
    assert_eq!(std::fs::read(&secret_path).unwrap(), b"g".repeat(64));
}

#[tokio::test]
async fn status_node_data_dir_selects_the_discovery_secret_host() {
    let scratch = tempfile::tempdir().unwrap();
    let seed = [1; 32];
    let public = install_discovery_secret(
        scratch.path(),
        &seed,
        &outbe_simplex::bls::KeyBackend::Plaintext,
    );
    let secret = std::fs::read(scratch.path().join("discovery-secret")).unwrap();
    assert_eq!(secret, hex::encode(seed).into_bytes());
    assert!(!outbe_simplex::bls::reads_as_protected_custody(&secret));
    let args = StatusArgs {
        node_data_dir: Some(scratch.path().to_path_buf()),
        ..status_args()
    };
    let mut rpc = status_rpc();
    bind_node(&mut rpc, NodeBindingSelectorV1::NodeHost(public), 2_000);
    let status = read_status(&rpc, &args).await.unwrap();
    assert_eq!(status["binding"]["validUntil"], 2_000);
    assert_eq!(status["lease"]["safeForNextFreeze"], true);
    assert!(status["lease"]["collateralValidUntil"].is_null());
    assert_eq!(
        std::fs::read(scratch.path().join("discovery-secret")).unwrap(),
        secret
    );
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn status_reth_plaintext_mode_644_selects_the_secp256k1_generator() {
    // SEC1 compressed generator. This is the standard vector for scalar 1.
    const GENERATOR: [u8; 33] = [
        0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87,
        0x0b, 0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16,
        0xf8, 0x17, 0x98,
    ];
    let scalar = b"0000000000000000000000000000000000000000000000000000000000000001";
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("discovery-secret");
    write_exact_mode(&path, scalar, 0o644);
    assert_eq!(file_mode(&path), 0o644);
    let args = StatusArgs {
        node_data_dir: Some(scratch.path().to_path_buf()),
        ..status_args()
    };
    let mut rpc = status_rpc();
    bind_node(&mut rpc, NodeBindingSelectorV1::NodeHost(GENERATOR), 2_100);
    let status = read_status(&rpc, &args).await.unwrap();
    assert_eq!(status["binding"]["validUntil"], 2_100);
    assert_eq!(status["lease"]["safeForNextFreeze"], true);
    assert_eq!(std::fs::read(&path).unwrap(), scalar);
    assert_eq!(file_mode(&path), 0o644);
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn status_reth_writer_discovery_secret_selects_the_libsecp256k1_public_key() {
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("discovery-secret");
    let secret = reth_node_core::args::NetworkArgs::default()
        .secret_key(path.clone())
        .unwrap();
    let public = secret.public_key(secp256k1::SECP256K1).serialize();
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(raw.len(), 64);
    assert!(!raw.contains(&b'\n'));
    let before_mode = file_mode(&path);
    let args = StatusArgs {
        node_data_dir: Some(scratch.path().to_path_buf()),
        ..status_args()
    };
    let mut rpc = status_rpc();
    bind_node(&mut rpc, NodeBindingSelectorV1::NodeHost(public), 2_200);
    let status = read_status(&rpc, &args).await.unwrap();
    assert_eq!(status["binding"]["validUntil"], 2_200);
    assert_eq!(status["lease"]["safeForNextFreeze"], true);
    assert_eq!(std::fs::read(&path).unwrap(), raw);
    let after_mode = file_mode(&path);
    assert_eq!(after_mode, before_mode);
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn status_rejects_newline_suffix_on_plaintext_discovery_secret() {
    let scalar = b"0000000000000000000000000000000000000000000000000000000000000001";
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("discovery-secret");
    let mut with_newline = scalar.to_vec();
    with_newline.push(b'\n');
    write_exact_mode(&path, &with_newline, 0o644);
    let args = StatusArgs {
        node_data_dir: Some(scratch.path().to_path_buf()),
        ..status_args()
    };
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::InvalidDiscoverySecret(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), with_newline);
    assert_eq!(file_mode(&path), 0o644);
}

#[tokio::test]
async fn status_rejects_discovery_secret_longer_than_read_limit() {
    let scalar = b"0000000000000000000000000000000000000000000000000000000000000001";
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("discovery-secret");
    let mut overlong = scalar.to_vec();
    overlong.resize(4097, b'0');
    write_exact_mode(&path, &overlong, 0o644);
    let args = StatusArgs {
        node_data_dir: Some(scratch.path().to_path_buf()),
        ..status_args()
    };
    let error = read_status(&MockRpc::default(), &args).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(TeeStatusError::InvalidDiscoverySecret(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), overlong);
    assert_eq!(file_mode(&path), 0o644);
}

fn file_mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn status_explicit_backend_accepts_mode_600_plaintext_discovery_secret() {
    const GENERATOR: [u8; 33] = [
        0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87,
        0x0b, 0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16,
        0xf8, 0x17, 0x98,
    ];
    let scalar = b"0000000000000000000000000000000000000000000000000000000000000001";
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("discovery-secret");
    write_exact_mode(&path, scalar, 0o600);
    for (backend, passphrase) in [
        ("encrypted", Some("unused-passphrase".to_owned())),
        ("os-level", None),
    ] {
        let args = StatusArgs {
            node_data_dir: Some(scratch.path().to_path_buf()),
            reth_key_backend: backend.to_owned(),
            reth_passphrase: passphrase,
            ..status_args()
        };
        let mut rpc = status_rpc();
        bind_node(&mut rpc, NodeBindingSelectorV1::NodeHost(GENERATOR), 2_100);
        let status = read_status(&rpc, &args).await.unwrap();
        assert_eq!(status["binding"]["validUntil"], 2_100, "{backend}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), scalar);
    assert_eq!(file_mode(&path), 0o600);
}

#[tokio::test]
async fn status_encrypted_discovery_secret_selects_the_independent_node_id() {
    let scratch = tempfile::tempdir().unwrap();
    let seed = [9; 32];
    let backend = outbe_simplex::bls::KeyBackend::Encrypted("custody-passphrase".to_owned());
    let public = install_discovery_secret(scratch.path(), &seed, &backend);
    let secret = std::fs::read(scratch.path().join("discovery-secret")).unwrap();
    assert!(outbe_simplex::bls::reads_as_protected_custody(&secret));
    assert_ne!(secret, hex::encode(seed).into_bytes());
    let args = StatusArgs {
        node_data_dir: Some(scratch.path().to_path_buf()),
        reth_key_backend: "encrypted".to_owned(),
        reth_passphrase: Some("custody-passphrase".to_owned()),
        ..status_args()
    };
    let mut rpc = status_rpc();
    bind_node(&mut rpc, NodeBindingSelectorV1::NodeHost(public), 2_500);
    let status = read_status(&rpc, &args).await.unwrap();
    assert_eq!(status["binding"]["validUntil"], 2_500);
    assert_eq!(status["lease"]["safeForNextFreeze"], true);
    assert_eq!(
        std::fs::read(scratch.path().join("discovery-secret")).unwrap(),
        secret
    );
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn status_explicit_node_id_and_validator_select_that_binding() {
    let node_id: [u8; 33] = k256::ecdsa::SigningKey::from_slice(&[2; 32])
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let mut rpc = status_rpc();
    bind_node(&mut rpc, NodeBindingSelectorV1::NodeHost(node_id), 3_000);
    let args = StatusArgs {
        node_id: Some(node_id),
        ..status_args()
    };
    let status = read_status(&rpc, &args).await.unwrap();
    assert_eq!(status["binding"]["validUntil"], 3_000);
    assert!(status["lease"]["collateralValidUntil"].is_null());

    let validator = Address::repeat_byte(0x11);
    let mut rpc = status_rpc();
    bind_node(&mut rpc, NodeBindingSelectorV1::Validator(validator), 4_000);
    let args = StatusArgs {
        validator: Some(validator),
        ..status_args()
    };
    let status = read_status(&rpc, &args).await.unwrap();
    assert_eq!(status["binding"]["validUntil"], 4_000);
}

#[test]
fn status_cli_accepts_one_binding_selector() {
    use clap::Parser;

    #[derive(Parser)]
    struct Tee {
        #[command(subcommand)]
        cmd: crate::commands::tee::TeeCmd,
    }

    let node_id = hex::encode(
        k256::ecdsa::SigningKey::from_slice(&[2; 32])
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes(),
    );
    match Tee::try_parse_from(["tee", "status", "--node-data-dir", "/tmp/node"])
        .unwrap()
        .cmd
    {
        crate::commands::tee::TeeCmd::Status(args) => {
            assert_eq!(
                args.node_data_dir.unwrap(),
                std::path::PathBuf::from("/tmp/node")
            );
            assert!(args.node_id.is_none() && args.validator.is_none());
        }
    }

    match Tee::try_parse_from(["tee", "status", "--node-id", &node_id])
        .unwrap()
        .cmd
    {
        crate::commands::tee::TeeCmd::Status(args) => {
            assert!(args.node_id.is_some());
            assert!(args.node_data_dir.is_none() && args.validator.is_none());
        }
    }

    match Tee::try_parse_from([
        "tee",
        "status",
        "--validator",
        "0x1111111111111111111111111111111111111111",
    ])
    .unwrap()
    .cmd
    {
        crate::commands::tee::TeeCmd::Status(args) => {
            assert_eq!(args.validator, Some(Address::repeat_byte(0x11)));
        }
    }
    assert!(Tee::try_parse_from([
        "tee",
        "status",
        "--node-id",
        &node_id,
        "--validator",
        "0x1111111111111111111111111111111111111111",
    ])
    .is_err());

    match Tee::try_parse_from(["tee", "status", "--node-id", "zz"]) {
        Err(error) => assert!(error_chain_has_hex_source(&error)),
        Ok(_) => panic!("bad node id was accepted"),
    }
}

fn error_chain_has_hex_source(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<hex::FromHexError>().is_some()
        || error.source().is_some_and(error_chain_has_hex_source)
}
