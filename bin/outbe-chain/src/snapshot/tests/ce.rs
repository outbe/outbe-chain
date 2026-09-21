use std::sync::Arc;

use alloy_consensus::{Header, Sealable};
use alloy_primitives::B256;
use outbe_compressed_entities::{
    sealed_root, AuthenticatedParentTree, CeAuditError, CeAuditLimits, CeAuditVisitor, CeAuditWork,
    CeMdbx, CeMdbxReadOnly, CeTopologyV1, Commitment, EntityRef, EnvironmentIdentity,
    ExactParentIdentity, FinalLeafMutation, FinalizedMarker, LeafValue, MdbxAuthenticatedTree,
    TreeKey, TreeNamespace, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_primitives::{
    reshare_artifact::{
        encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
    },
    time::WorldwideDay,
    OutbeHeader,
};

use super::super::validation::ce::verify_ce;

struct Fixture {
    _source: tempfile::TempDir,
    reader: CeMdbxReadOnly,
    header: OutbeHeader,
}

impl Fixture {
    fn new(populated: bool) -> Self {
        Self::with_corrupt_leaf(populated, false)
    }

    fn with_corrupt_leaf(populated: bool, corrupt_leaf: bool) -> Self {
        let source = tempfile::tempdir().unwrap();
        let mut header = OutbeHeader::new(Header::default());
        let identity = EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 10,
            genesis_hash: header.hash_slow(),
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
        };
        let marker = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: header.hash_slow(),
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: sealed_root(B256::ZERO).unwrap(),
        };
        let db = Arc::new(CeMdbx::open(source.path(), identity.clone(), marker).unwrap());
        if populated {
            let parent = MdbxAuthenticatedTree::open(
                db.clone(),
                ExactParentIdentity {
                    commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                    block_number: 0,
                    block_hash: marker.block_hash,
                    root: marker.new_root,
                },
            )
            .unwrap();
            let id = WwdEntityId::from_day_and_digest(WorldwideDay::new(20_260_718), [7; 32]);
            let mutations: Vec<_> = [
                EntityRef::Tribute(id),
                EntityRef::NodItem(id),
                EntityRef::NodBucket(id),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, entity)| FinalLeafMutation {
                entity,
                final_leaf: Some(
                    Commitment::try_from(B256::with_last_byte(index as u8 + 1).0).unwrap(),
                ),
            })
            .collect();
            let prepared = parent.prepare_seal(1, &mutations, &[]).unwrap();
            header = OutbeHeader::new(Header {
                number: 1,
                parent_hash: marker.block_hash,
                extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                    compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        r_sealed: prepared.new_root(),
                    }),
                    ..Default::default()
                })
                .unwrap(),
                ..Default::default()
            });
            db.apply_finalized(&prepared.freeze(header.hash_slow()))
                .unwrap();
        }
        drop(db);
        if corrupt_leaf {
            use reth_ethereum::provider::db::{
                cursor::DbCursorRO,
                database::Database,
                mdbx::DatabaseArguments,
                table::Table,
                transaction::{DbTx, DbTxMut},
                DatabaseEnv, DatabaseEnvKind,
            };
            // Test-only native corruption: keep roots, marker and header untouched.
            // The production adapter has no access to this table descriptor.
            #[derive(Debug)]
            struct TestCeLeaves;
            impl Table for TestCeLeaves {
                const NAME: &'static str = "OutbeCompressedEntitiesLeavesV3";
                const DUPSORT: bool = false;
                type Key = Vec<u8>;
                type Value = Vec<u8>;
            }
            let env = DatabaseEnv::open(
                &source.path().join("compressed_entities/smt"),
                DatabaseEnvKind::RW,
                DatabaseArguments::test(),
            )
            .unwrap();
            let tx = env.tx_mut().unwrap();
            let (key, _) = tx
                .cursor_read::<TestCeLeaves>()
                .unwrap()
                .seek(vec![1])
                .unwrap()
                .unwrap();
            assert_eq!(key[0], 1);
            tx.put::<TestCeLeaves>(key, B256::with_last_byte(42).to_vec())
                .unwrap();
            tx.commit().unwrap();
        }
        std::fs::write(source.path().join("protected-config"), b"unchanged").unwrap();
        let reader = CeMdbxReadOnly::open(source.path(), identity).unwrap();
        Self {
            _source: source,
            reader,
            header,
        }
    }
}

#[derive(Default)]
struct Count(u64);
impl CeAuditVisitor for Count {
    fn visit_leaf(
        &mut self,
        _: TreeNamespace,
        _: TreeKey,
        _: LeafValue,
    ) -> Result<(), CeAuditError> {
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn native_ce_audit_binds_genesis_and_all_nonzero_domains_to_their_exact_header() {
    for populated in [false, true] {
        let fixture = Fixture::new(populated);
        let scratch = tempfile::tempdir().unwrap();
        let work = CeAuditWork::create(
            scratch.path().join("audit"),
            CeAuditLimits {
                records_per_run: 2,
                merge_fan_in: 2,
            },
        )
        .unwrap();
        let before = fixture.reader.marker().unwrap();
        let files = super::headers::fingerprint(fixture._source.path());
        let mut visitor = Count::default();
        let report = verify_ce(&fixture.reader, &fixture.header, &work, &mut visitor).unwrap();
        assert_eq!(report.sealed_root, before.new_root);
        assert_eq!(report.leaves, if populated { 6 } else { 0 });
        assert_eq!(visitor.0, report.leaves);
        assert_eq!(fixture.reader.marker().unwrap(), before);
        assert_eq!(super::headers::fingerprint(fixture._source.path()), files);
    }
}

#[test]
fn ce_audit_does_not_substitute_a_different_header_or_ignore_callback_failure() {
    let fixture = Fixture::new(true);
    let files = super::headers::fingerprint(fixture._source.path());
    let scratch = tempfile::tempdir().unwrap();
    let work = CeAuditWork::create(scratch.path().join("audit"), CeAuditLimits::default()).unwrap();
    let mut wrong = fixture.header.clone();
    wrong.inner.timestamp += 1;
    let mut visitor = Count::default();
    assert!(verify_ce(&fixture.reader, &wrong, &work, &mut visitor).is_err());
    assert_eq!(visitor.0, 0);

    struct Interrupted;
    impl CeAuditVisitor for Interrupted {
        fn visit_leaf(
            &mut self,
            _: TreeNamespace,
            _: TreeKey,
            _: LeafValue,
        ) -> Result<(), CeAuditError> {
            Err(CeAuditError::Invalid("body sink interrupted".into()))
        }
    }
    let error = verify_ce(&fixture.reader, &fixture.header, &work, &mut Interrupted).unwrap_err();
    assert!(error.to_string().contains("body sink interrupted"));
    assert_eq!(super::headers::fingerprint(fixture._source.path()), files);
}

#[test]
fn ce_audit_detects_changed_native_leaf_with_unchanged_marker_and_header() {
    let fixture = Fixture::with_corrupt_leaf(true, true);
    let marker = fixture.reader.marker().unwrap();
    super::super::validation::headers::verify_header_ce_marker(&fixture.header, &marker).unwrap();
    let files = super::headers::fingerprint(fixture._source.path());
    let scratch = tempfile::tempdir().unwrap();
    let work = CeAuditWork::create(scratch.path().join("audit"), CeAuditLimits::default()).unwrap();
    let error = verify_ce(
        &fixture.reader,
        &fixture.header,
        &work,
        &mut Count::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("rebuilt root differs"));
    assert_eq!(fixture.reader.marker().unwrap(), marker);
    assert_eq!(super::headers::fingerprint(fixture._source.path()), files);
}
