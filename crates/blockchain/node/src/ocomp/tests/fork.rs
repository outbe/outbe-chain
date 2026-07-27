use outbe_metadosis::ocomp::{
    fork::OcompForkInstallClassification, schema::poc_schema_limits,
    test_support::fork_install_fixture,
};
use outbe_primitives::OutbeHeader;
use reth_chainspec::ChainSpec;
use serde_json::json;

use crate::ocomp::fork::{load_ocomp_fork_install, OCOMP_FORK_INSTALL_GENESIS_KEY};

#[test]
fn genesis_fork_install_loader_accepts_one_exact_binding_and_rejects_hash_mismatch() {
    let mut spec = ChainSpec::<OutbeHeader>::default();
    let install = fork_install_fixture(
        OcompForkInstallClassification::Measurement,
        32,
        spec.chain().id(),
        spec.genesis_hash(),
    );
    let limits = poc_schema_limits();
    let canonical_bytes = install.encode_canonical(&limits).unwrap();
    let install_hash = install.install_hash(&limits).unwrap();
    spec.genesis
        .config
        .extra_fields
        .insert_value(
            OCOMP_FORK_INSTALL_GENESIS_KEY.to_owned(),
            json!({
                "canonicalBytes": format!("0x{}", hex::encode(&canonical_bytes)),
                "installHash": install_hash,
            }),
        )
        .unwrap();

    let loaded = load_ocomp_fork_install(&spec).unwrap().unwrap();
    assert_eq!(loaded.as_ref(), &install);

    spec.genesis
        .config
        .extra_fields
        .insert_value(
            OCOMP_FORK_INSTALL_GENESIS_KEY.to_owned(),
            json!({
                "canonicalBytes": format!("0x{}", hex::encode(canonical_bytes)),
                "installHash": alloy_primitives::B256::repeat_byte(0x99),
            }),
        )
        .unwrap();
    assert!(load_ocomp_fork_install(&spec).is_err());

    let empty = ChainSpec::<OutbeHeader>::default();
    assert!(load_ocomp_fork_install(&empty).unwrap().is_none());
}
