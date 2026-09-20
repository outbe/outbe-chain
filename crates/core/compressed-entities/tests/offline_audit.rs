use alloy_primitives::B256;
use outbe_compressed_entities::{
    sealed_root, CeAuditError, CeAuditLimits, CeAuditVisitor, CeAuditWork, CeMdbx, CeMdbxReadOnly,
    CeTopologyV1, EnvironmentIdentity, ExactParentIdentity, FinalizedMarker, LeafValue, TreeKey,
    TreeNamespace, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};

#[test]
fn external_consumer_can_name_every_tree_visitor_type_and_audit_genesis() {
    struct NoLeaves;
    impl CeAuditVisitor for NoLeaves {
        fn visit_leaf(
            &mut self,
            _: TreeNamespace,
            _: TreeKey,
            _: LeafValue,
        ) -> Result<(), CeAuditError> {
            panic!("native genesis contains no leaves");
        }
    }
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let hash = B256::repeat_byte(3);
    let identity = EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 10,
        genesis_hash: hash,
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
    };
    let marker = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    drop(CeMdbx::open(source.path(), identity.clone(), marker).unwrap());
    let reader = CeMdbxReadOnly::open(source.path(), identity).unwrap();
    let path = scratch.path().join("audit");
    let work = CeAuditWork::create(&path, CeAuditLimits::default()).unwrap();
    let report = reader
        .audit_exact(
            ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 0,
                block_hash: hash,
                root: marker.new_root,
            },
            &work,
            &mut NoLeaves,
        )
        .unwrap();
    assert_eq!(report.sealed_root, marker.new_root);
    assert_eq!((report.trees, report.leaves), (1, 0));
    drop(work);
    assert!(!path.exists());
    assert!(source
        .path()
        .join("compressed_entities/smt/mdbx.dat")
        .exists());
}
