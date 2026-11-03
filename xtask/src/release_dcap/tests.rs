use super::host::{field_value, lscpu_fields, parse_lscpu_u64};
use super::*;
use outbe_primitives::{
    chain::{OutbeNetwork, TESTNET_CHAIN_ID},
    tee_attestation_v1::{PlatformTcbStatusSetV1, QvlTcbStatusV1, TeePolicyV1},
    tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1, ProductionSgxMeasurementV1},
};

#[test]
fn release_cli_requires_an_explicit_production_network_and_genesis() {
    let mainnet = Cli::try_parse_from([
        "outbe-release-dcap-evidence",
        "--network",
        "mainnet",
        "--image",
        "ghcr.io/outbe/enclave@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--bundle",
        "/tmp/bundle",
        "--genesis",
        "/tmp/mainnet-genesis.json",
        "--expected-pck-ca",
        "processor",
        "--output-dir",
        "/tmp/evidence",
    ])
    .expect("Mainnet release CLI");
    assert_eq!(
        mainnet.network.artifact_set(),
        ReleaseDcapArtifactSetV1::Mainnet
    );
    assert_eq!(mainnet.genesis, PathBuf::from("/tmp/mainnet-genesis.json"));

    assert!(Cli::try_parse_from([
        "outbe-release-dcap-evidence",
        "--network",
        "devnet",
        "--image",
        "ghcr.io/outbe/enclave@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--bundle",
        "/tmp/bundle",
        "--genesis",
        "/tmp/genesis.json",
        "--expected-pck-ca",
        "processor",
        "--output-dir",
        "/tmp/evidence",
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "outbe-release-dcap-evidence",
        "--image",
        "ghcr.io/outbe/enclave@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--bundle",
        "/tmp/bundle",
        "--genesis",
        "/tmp/genesis.json",
        "--expected-pck-ca",
        "processor",
        "--output-dir",
        "/tmp/evidence",
    ])
    .is_err());
}

#[test]
fn hardware_release_chain_binding_is_closed_per_network() {
    assert!(ensure_release_chain_id(
        ReleaseDcapArtifactSetV1::Mainnet,
        outbe_primitives::chain::MAINNET_CHAIN_ID
    )
    .is_ok());
    assert!(ensure_release_chain_id(
        ReleaseDcapArtifactSetV1::Testnet,
        outbe_primitives::chain::TESTNET_CHAIN_ID
    )
    .is_ok());
    assert!(ensure_release_chain_id(
        ReleaseDcapArtifactSetV1::Mainnet,
        outbe_primitives::chain::TESTNET_CHAIN_ID
    )
    .is_err());
    assert!(ensure_release_chain_id(
        ReleaseDcapArtifactSetV1::Testnet,
        outbe_primitives::chain::MAINNET_CHAIN_ID
    )
    .is_err());
}

#[test]
fn release_artifact_sets_are_closed_and_differ_only_by_genesis() {
    let testnet = ReleaseDcapArtifactSetV1::for_network(OutbeNetwork::Testnet).unwrap();
    let mainnet = ReleaseDcapArtifactSetV1::for_network(OutbeNetwork::Mainnet).unwrap();
    assert!(ReleaseDcapArtifactSetV1::for_network(OutbeNetwork::Devnet).is_none());
    assert_eq!(testnet.paths().len(), 18);
    assert_eq!(mainnet.paths().len(), 18);
    assert!(testnet.paths().contains("testnet-genesis.json"));
    assert!(!testnet.paths().contains("mainnet-genesis.json"));
    assert!(mainnet.paths().contains("mainnet-genesis.json"));
    assert!(!mainnet.paths().contains("testnet-genesis.json"));

    let common = testnet
        .paths()
        .intersection(&mainnet.paths())
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(common.len(), 17);
}

fn testnet_policy(measurements: &Measurements) -> TeePolicyV1 {
    initial_tee_policy_v1(
        InitialTeeProfileV1::DcapRequired(ProductionSgxMeasurementV1 {
            mrenclave: parse_b256(&measurements.mrenclave).unwrap(),
            mrsigner: parse_b256(&measurements.mrsigner).unwrap(),
            isv_prod_id: measurements.isv_prod_id,
            minimum_isv_svn: measurements.isv_svn,
            minimum_tcb_evaluation_data_number: 1,
        }),
        TESTNET_CHAIN_ID,
        B256::repeat_byte(3),
    )
    .unwrap()
}

#[test]
fn exact_image_reference_requires_lowercase_sha256() {
    let digest = "a".repeat(64);
    assert_eq!(
        exact_image_digest(&format!("ghcr.io/outbe/enclave@sha256:{digest}"))
            .expect("exact digest"),
        digest
    );
    assert!(exact_image_digest("ghcr.io/outbe/enclave:latest").is_err());
    assert!(
        exact_image_digest(&format!("ghcr.io/outbe/enclave@sha256:{}", "A".repeat(64))).is_err()
    );
}

#[test]
fn records_lscpu_package_topology_as_untrusted_provenance() {
    let value = serde_json::json!({"lscpu": [
        {"field": "Model name:", "data": "Intel(R) Xeon(R)"},
        {"field": "Socket(s):", "data": "2"}
    ]});
    let fields = lscpu_fields(&value).expect("lscpu fields");
    assert_eq!(parse_lscpu_u64(&fields, &["Socket(s):"]).unwrap(), 2);
    assert_eq!(
        field_value(&fields, &["Model name:"]).unwrap(),
        "Intel(R) Xeon(R)"
    );
}

#[test]
fn testnet_policy_fixture_binds_signed_measurements_and_strict_statuses() {
    let measurements = Measurements {
        debug: false,
        isv_prod_id: 1,
        isv_svn: 2,
        mrenclave: "11".repeat(32),
        mrsigner: "22".repeat(32),
    };
    let policy = testnet_policy(&measurements);
    assert_eq!(
        policy.accepted_platform_tcb_statuses,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded
    );
    assert_eq!(policy.accepted_qe_tcb_status, QvlTcbStatusV1::UpToDate);
    assert_eq!(policy.measurement_rules.len(), 1);
    assert_eq!(
        policy.measurement_rules[0].mrenclave,
        B256::repeat_byte(0x11)
    );
    assert_eq!(
        policy.measurement_rules[0].mrsigner,
        B256::repeat_byte(0x22)
    );
}
