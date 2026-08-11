use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "ocomp-integration")]
use outbe_chain_constants::DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS;
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::profile::{poc_schema_limits, CapacityProfileV1};
#[cfg(feature = "ocomp-integration")]
use sha2::{Digest as _, Sha256};

const VALIDATOR_COUNT: usize = 4;
const ROOT_FILES: &[&str] = &[
    "dkg-output.hex",
    "genesis.json",
    "polynomial.hex",
    "reth-bootnodes.txt",
    "validators.json",
];
const VALIDATOR_FILES: &[&str] = &[
    "evm-key.hex",
    "reth-p2p-secret.hex",
    "signing-key.hex",
    "signing-share.hex",
];
#[cfg(feature = "ocomp-integration")]
const ARTIFACT_FILES: &[&str] = &[
    "capacity-profile-v1.ocb1",
    "correctness-profile-v1.ocb1",
    "fork-install-v1.ocb1",
    "generated-capacity-v1.json",
    "genesis-final.json",
    "network-binding-v1.json",
    "protocol-bundle-v1.ocb1",
    "semantic-artifacts-v1.json",
];

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("ocomp-final-v1")
        .join("base")
}

#[cfg(feature = "ocomp-integration")]
fn artifact_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("ocomp-final-v1")
        .join("artifacts")
}

#[test]
fn canonical_ocomp_base_fixture_has_only_bootstrap_authorities() {
    let root = fixture_root();
    for relative in ROOT_FILES {
        require_regular_file(&root.join(relative));
    }
    for validator_index in 0..VALIDATOR_COUNT {
        let validator = root.join(format!("validator-{validator_index}"));
        for relative in VALIDATOR_FILES {
            require_regular_file(&validator.join(relative));
        }
    }

    let validators: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("validators.json")).unwrap()).unwrap();
    let validators = validators
        .as_array()
        .expect("validators manifest is an array");
    assert_eq!(validators.len(), VALIDATOR_COUNT);
    let addresses = validators
        .iter()
        .map(|entry| {
            entry
                .get("address")
                .and_then(serde_json::Value::as_str)
                .expect("validator address")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(addresses.len(), VALIDATOR_COUNT);

    let genesis: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("genesis.json")).unwrap()).unwrap();
    let config = genesis
        .get("config")
        .and_then(serde_json::Value::as_object)
        .expect("base genesis config");
    assert!(
        !config.contains_key("ocompForkInstallV1"),
        "base fixture must remain unarmed until final-artifact generation"
    );
    assert_genesis_epoch_matches_validator_set(&genesis);
    assert_eq!(
        outbe_chain_constants::GenesisProtocolParametersV1::from_materialized_genesis(&genesis)
            .expect("canonical base genesis materializes immutable chain constants"),
        outbe_chain_constants::GenesisProtocolParametersV1::default(),
        "canonical fixture without explicit overrides must materialize production defaults"
    );

    for forbidden in ["data", "logs", "ocomp", "tee", "node.log", "enclave.log"] {
        assert!(
            !root.join(forbidden).exists(),
            "runtime state leaked into canonical base fixture: {forbidden}"
        );
    }
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_final_artifacts_are_complete_hash_bound_and_node_loadable() {
    let root = artifact_root();
    for relative in ARTIFACT_FILES {
        require_regular_file(&root.join(relative));
    }

    let capacity_bytes = fs::read(root.join("generated-capacity-v1.json")).unwrap();
    let capacity: serde_json::Value = serde_json::from_slice(&capacity_bytes).unwrap();
    let capacity_profile_bytes = hex::decode(
        capacity["capacity_profile_ocb1_hex"]
            .as_str()
            .expect("capacity profile canonical hex"),
    )
    .unwrap();
    let mut capacity_profile =
        CapacityProfileV1::decode_canonical(&capacity_profile_bytes, &poc_schema_limits()).unwrap();
    if capacity_profile.result_deadline_blocks != DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS {
        capacity_profile.result_deadline_blocks = DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS;
        let expected = capacity_profile
            .encode_canonical(&poc_schema_limits())
            .unwrap();
        panic!(
            "stale capacity response window: expected canonical_hex={} sha256=0x{}",
            hex::encode(&expected),
            hex::encode(Sha256::digest(&expected))
        );
    }
    assert_eq!(
        capacity["capacity_profile"]["result_deadline_blocks"],
        DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        "retained capacity JSON must expose the exact protocol response window"
    );
    let network: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("network-binding-v1.json")).unwrap()).unwrap();
    assert_eq!(network["classification"], "final");
    assert_eq!(network["activation_height"], 32);
    assert_eq!(
        network["generated_capacity_manifest_sha256"],
        format!("0x{}", hex::encode(Sha256::digest(&capacity_bytes))),
        "network binding must identify the retained capacity manifest"
    );

    let genesis_path = root.join("genesis-final.json");
    let genesis: serde_json::Value =
        serde_json::from_slice(&fs::read(&genesis_path).unwrap()).unwrap();
    assert_genesis_epoch_matches_validator_set(&genesis);
    assert_eq!(
        outbe_chain_constants::GenesisProtocolParametersV1::from_materialized_genesis(&genesis)
            .expect("canonical Final genesis materializes immutable chain constants"),
        outbe_chain_constants::GenesisProtocolParametersV1::default(),
        "Final artifact must preserve the base fixture's chain constants"
    );
    let chain_spec = reth_ethereum::cli::chainspec::chain_value_parser(
        genesis_path.to_str().expect("fixture path is UTF-8"),
    )
    .unwrap()
    .as_ref()
    .clone()
    .map_header(outbe_primitives::OutbeHeader::new);
    let install = outbe_node::ocomp::fork::load_ocomp_fork_install(&chain_spec)
        .unwrap()
        .expect("Final genesis contains an OCOMP install");
    assert_eq!(
        install.classification,
        outbe_metadosis::config::OcompForkInstallClassification::Final
    );
    assert_eq!(install.activation_height, 32);
    for obsolete in [
        "result-committee-public-v1.json",
        "result-committee-v1.ocb1",
    ] {
        assert!(
            !root.join(obsolete).exists(),
            "canonical install must not retain independent OCOMP membership: {obsolete}"
        );
    }
    for obsolete_binding in [
        "result_committee_snapshot_hash",
        "result_committee_ocb1_sha256",
    ] {
        assert!(
            network.get(obsolete_binding).is_none(),
            "network binding must not retain independent OCOMP membership: {obsolete_binding}"
        );
    }
    assert_eq!(
        install
            .request_profile
            .capacity_profile
            .max_tributes_per_work_shard,
        256
    );
}

fn assert_genesis_epoch_matches_validator_set(genesis: &serde_json::Value) {
    const VALIDATOR_SET: &str = "000000000000000000000000000000000000ee00";
    const EPOCH_LENGTH_SLOT: &str =
        "0x0000000000000000000000000000000000000000000000000000000000000002";

    let configured = genesis["config"]["epochLengthBlocks"]
        .as_u64()
        .expect("positive genesis config epochLengthBlocks");
    let stored_hex = genesis["alloc"][VALIDATOR_SET]["storage"][EPOCH_LENGTH_SLOT]
        .as_str()
        .expect("ValidatorSet epoch length storage slot");
    let stored = u64::from_str_radix(stored_hex.trim_start_matches("0x"), 16)
        .expect("ValidatorSet epoch length is canonical hex");

    assert_eq!(
        stored, configured,
        "canonical genesis must seed ValidatorSet epoch length from config"
    );
}

fn require_regular_file(path: &Path) {
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("inspect fixture {}: {error}", path.display()));
    assert!(
        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        "fixture authority is not a regular non-symlink file: {}",
        path.display()
    );
    assert!(
        metadata.len() > 0,
        "fixture authority is empty: {}",
        path.display()
    );
}
