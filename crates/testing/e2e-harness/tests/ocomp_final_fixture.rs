use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("ocomp-final-v1")
        .join("base")
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

    for forbidden in ["data", "logs", "ocomp", "tee", "node.log", "enclave.log"] {
        assert!(
            !root.join(forbidden).exists(),
            "runtime state leaked into canonical base fixture: {forbidden}"
        );
    }
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
