use std::fs;

use alloy_consensus::{Header, Sealable};
use alloy_primitives::B256;
use outbe_compressed_entities::{sealed_root, FinalizedMarker, ACTIVE_COMMITMENT_SCHEME};
use outbe_primitives::reshare_artifact::{
    encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
};
use outbe_primitives::{OutbeHeader, OutbePrimitives};
use outbe_snapshot::manifest::BlockIdentity;
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    open_db_read_only,
    table::Table,
    tables::{self, ChainStateKey},
    transaction::{DbTx, DbTxMut},
    ClientVersion,
};
use reth_provider::{
    providers::{StaticFileProvider, StaticFileProviderBuilder},
    StaticFileSegment, StaticFileWriter,
};

use super::super::{
    config::{parse_node_inputs, resolve_layout, NativeLayout},
    native::{RethProgress, RethReadOnlyView},
    validation::headers::{
        read_header_ce_commitment, verify_header_ce_marker, verify_retained_headers, HeaderAudit,
    },
};

type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;

struct Fixture {
    _root: tempfile::TempDir,
    layout: NativeLayout,
    headers: Vec<OutbeHeader>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_static_hash(None)
    }

    fn with_static_hash(corrupt: Option<u64>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let inputs = parse_node_inputs(super::layout::native_arguments(root.path())).unwrap();
        let layout = resolve_layout(&inputs).unwrap();
        fs::create_dir_all(&layout.static_files_root).unwrap();
        let mut headers = vec![layout.chain.genesis_header().clone()];
        for number in 1..=9 {
            headers.push(OutbeHeader::new(Header {
                number,
                parent_hash: headers.last().unwrap().hash_slow(),
                ..Default::default()
            }));
        }
        let static_files = StaticFileProviderBuilder::read_write(&layout.static_files_root)
            .with_blocks_per_file(2)
            .build::<OutbePrimitives>()
            .unwrap();
        {
            let mut writer = static_files
                .get_writer(0, StaticFileSegment::Headers)
                .unwrap();
            for header in &headers[..8] {
                let hash = if corrupt == Some(header.inner.number) {
                    B256::repeat_byte(99)
                } else {
                    header.hash_slow()
                };
                writer.append_header(header, &hash).unwrap();
            }
        }
        static_files.commit().unwrap();
        static_files
            .delete_segment_below_block(StaticFileSegment::Headers, 4)
            .unwrap();
        drop(static_files);

        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        for header in &headers[8..] {
            tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header.clone())
                .unwrap();
            tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
                .unwrap();
        }
        tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 8)
            .unwrap();
        tx.put::<tables::StageCheckpoints>("Execution".into(), StageCheckpoint::new(9))
            .unwrap();
        tx.put::<tables::StageCheckpoints>("Finish".into(), StageCheckpoint::new(9))
            .unwrap();
        tx.commit().unwrap();
        drop(db);
        Self {
            _root: root,
            layout,
            headers,
        }
    }

    fn view(&self) -> RethReadOnlyView {
        let identity = |number: usize| BlockIdentity {
            number: number as u64,
            hash: hex::encode(self.headers[number].hash_slow()),
        };
        RethReadOnlyView {
            db: open_db_read_only(
                self.layout.chain_root.join("db"),
                DatabaseArguments::new(ClientVersion::default()),
            )
            .unwrap(),
            static_files: StaticFileProvider::<OutbePrimitives>::read_only(
                &self.layout.static_files_root,
            )
            .unwrap(),
            chain: self.layout.chain.clone(),
            protected: self.layout.protected.clone(),
            progress: RethProgress {
                finalized: identity(8),
                execution: identity(9),
                execution_stage: Some(9),
                finish_stage: Some(9),
                partial_state_trie: None,
                unwind: None,
                storage_version: 1,
            },
        }
    }

    fn audit(&self, required: &[u64]) -> eyre::Result<HeaderAudit> {
        let before = fingerprint(self._root.path());
        let result = verify_retained_headers(&self.view(), required);
        assert_eq!(fingerprint(self._root.path()), before);
        result
    }
}

// Cover the entire fixture, including configuration and key sentinels. Preserve
// membership, size and modes even for MDBX's existing reader bookkeeping file;
// only its mutable reader-slot bytes are excluded from the digest comparison.
pub(super) fn fingerprint(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, (bool, u64, u32, u64)> {
    use std::{hash::Hasher, io::Read, os::unix::fs::PermissionsExt};
    let mut pending = vec![root.to_path_buf()];
    let mut found = std::collections::BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let is_directory = metadata.is_dir();
            let mut digest = std::hash::DefaultHasher::new();
            if is_directory {
                pending.push(path.clone());
            } else if entry.file_name() != "mdbx.lck" {
                assert!(metadata.is_file());
                let mut file = fs::File::open(&path).unwrap();
                let mut buffer = [0; 65536];
                loop {
                    let count = file.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    digest.write(&buffer[..count]);
                }
            }
            found.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (
                    is_directory,
                    metadata.len(),
                    metadata.permissions().mode(),
                    digest.finish(),
                ),
            );
        }
    }
    found
}

#[test]
fn absent_static_segment_is_a_gap_and_does_not_join_unrelated_parents() {
    let fixture = Fixture::new();
    let static_files = StaticFileProviderBuilder::read_write(&fixture.layout.static_files_root)
        .with_blocks_per_file(2)
        .build::<OutbePrimitives>()
        .unwrap();
    static_files
        .delete_jar(StaticFileSegment::Headers, 6)
        .unwrap();
    drop(static_files);
    let audit = fixture.audit(&[6, 7, 8]).unwrap();
    assert_eq!(audit.intervals, vec![4..=5, 8..=9]);
    assert_eq!(audit.verified_headers, 4);
    assert_eq!(audit.required_missing, vec![6, 7]);
}

#[test]
fn retained_static_hash_is_recomputed_from_native_header() {
    let fixture = Fixture::with_static_hash(Some(5));
    let error = fixture.audit(&[]).unwrap_err();
    assert!(error.to_string().contains("hash"), "{error:#}");
}

#[test]
fn static_metadata_must_describe_exactly_the_retained_rows() {
    for (start, end) in [(6, 6), (5, 7)] {
        let fixture = Fixture::new();
        let static_files = StaticFileProviderBuilder::read_write(&fixture.layout.static_files_root)
            .with_blocks_per_file(2)
            .build::<OutbePrimitives>()
            .unwrap();
        {
            let mut writer = static_files
                .get_writer(7, StaticFileSegment::Headers)
                .unwrap();
            writer.user_header_mut().set_block_range(start, end);
        }
        static_files.commit().unwrap();
        drop(static_files);
        let error = fixture.audit(&[]).unwrap_err();
        assert!(
            error.to_string().contains("range") || error.to_string().contains("rows"),
            "{error:#}"
        );
    }
}

fn ce_header() -> (OutbeHeader, FinalizedMarker) {
    let root = B256::repeat_byte(42);
    let header = OutbeHeader::new(Header {
        number: 8,
        parent_hash: B256::repeat_byte(7),
        extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                r_sealed: root,
            }),
            ..Default::default()
        })
        .unwrap(),
        ..Default::default()
    });
    let marker = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 8,
        block_hash: header.hash_slow(),
        parent_block_hash: header.inner.parent_hash,
        parent_root: B256::repeat_byte(41),
        new_root: root,
    };
    (header, marker)
}

#[test]
fn ce_root_binds_to_exact_marker_height_hash_and_root() {
    let (header, marker) = ce_header();
    verify_header_ce_marker(&header, &marker).unwrap();
    for mutation in 0..4 {
        let mut bad = marker;
        match mutation {
            0 => bad.height += 1,
            1 => bad.block_hash = B256::repeat_byte(99),
            2 => bad.new_root = B256::repeat_byte(99),
            _ => bad.commitment_scheme_version += 1,
        }
        assert!(verify_header_ce_marker(&header, &bad).is_err());
    }
}

#[test]
fn missing_ce_artifact_is_not_a_fabricated_zero_commitment() {
    let (mut header, mut marker) = ce_header();
    for extra_data in [
        Default::default(),
        encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap(),
    ] {
        header.inner.extra_data = extra_data;
        marker.block_hash = header.hash_slow();
        assert_eq!(read_header_ce_commitment(&header).unwrap(), None);
        assert!(verify_header_ce_marker(&header, &marker).is_err());
    }
}

#[test]
fn ce_decoder_rejects_duplicate_trailing_unknown_version_and_wrong_scheme() {
    let (header, _) = ce_header();
    let valid = header.inner.extra_data.to_vec();
    let mut duplicate = valid.clone();
    duplicate[5] += 1;
    duplicate.extend_from_slice(&valid[6..]);
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut version = valid;
    version[4] = 0;
    let wrong_scheme = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        compressed_entities_root: Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME + 1,
            r_sealed: B256::repeat_byte(42),
        }),
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    for bytes in [duplicate, trailing, version, wrong_scheme] {
        let mut bad = header.clone();
        bad.inner.extra_data = bytes.into();
        assert!(read_header_ce_commitment(&bad).is_err());
    }
}

#[test]
fn genesis_uses_native_empty_catalog_seal_without_inventing_an_artifact() {
    let root = tempfile::tempdir().unwrap();
    let inputs = parse_node_inputs(super::layout::native_arguments(root.path())).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    let header = layout.chain.genesis_header();
    let marker = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: header.hash_slow(),
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    verify_header_ce_marker(header, &marker).unwrap();
    let mut wrong = marker;
    wrong.new_root = B256::ZERO;
    assert!(verify_header_ce_marker(header, &wrong).is_err());
    wrong = marker;
    wrong.block_hash = B256::repeat_byte(99);
    assert!(verify_header_ce_marker(header, &wrong).is_err());
}

#[test]
fn pruned_static_segments_join_the_mdbx_tail_without_genesis_traversal() {
    let fixture = Fixture::new();
    let view = fixture.view();
    assert_eq!(
        view.static_files
            .get_lowest_range_start(StaticFileSegment::Headers),
        Some(4)
    );
    drop(view);
    let audit = fixture.audit(&[0, 3, 4, 7, 8, 9, 10]).unwrap();
    assert_eq!(audit.intervals, vec![4..=9]);
    assert_eq!(audit.verified_headers, 6);
    assert_eq!(audit.required_missing, vec![0, 3, 10]);
}

#[test]
fn missing_header_inside_retained_canonical_rows_is_not_a_retention_boundary() {
    let fixture = Fixture::new();
    let db = init_db(
        fixture.layout.chain_root.join("db"),
        DatabaseArguments::test(),
    )
    .unwrap();
    let tx = db.tx_mut().unwrap();
    tx.delete::<tables::Headers<OutbeHeader>>(8, None).unwrap();
    tx.commit().unwrap();
    drop(db);
    let error = fixture.audit(&[8]).unwrap_err();
    assert!(error.to_string().contains('8'), "{error:#}");
}

#[test]
fn contiguous_static_to_mdbx_parent_mismatch_is_rejected() {
    let fixture = Fixture::new();
    let db = init_db(
        fixture.layout.chain_root.join("db"),
        DatabaseArguments::test(),
    )
    .unwrap();
    let tx = db.tx_mut().unwrap();
    let mut header = fixture.headers[8].clone();
    header.inner.parent_hash = B256::repeat_byte(99);
    tx.put::<tables::CanonicalHeaders>(8, header.hash_slow())
        .unwrap();
    tx.put::<tables::Headers<OutbeHeader>>(8, header).unwrap();
    tx.commit().unwrap();
    drop(db);
    let error = fixture.audit(&[]).unwrap_err();
    assert!(error.to_string().contains("parent"), "{error:#}");
}

#[test]
fn native_read_only_open_accepts_pruned_genesis_and_observes_current_progress() {
    let fixture = Fixture::new();
    let before = fingerprint(fixture._root.path());
    let view = RethReadOnlyView::open(&fixture.layout).unwrap();
    assert!(view.header(0).unwrap().is_none());
    assert!(view.canonical_hash(0).unwrap().is_none());
    assert_eq!(view.progress.finalized.number, 8);
    assert_eq!(view.progress.execution.number, 9);
    assert_eq!(
        view.progress.execution.hash,
        hex::encode(fixture.headers[9].hash_slow())
    );
    let audit = verify_retained_headers(&view, &[0, 8, 9]).unwrap();
    assert_eq!(audit.required_missing, vec![0]);
    assert_eq!(audit.intervals, vec![4..=9]);
    drop(view);
    assert_eq!(fingerprint(fixture._root.path()), before);
}

#[test]
fn mdbx_number_hash_and_missing_canonical_corruption_are_rejected() {
    for mutation in 0..3 {
        let fixture = Fixture::new();
        let db = init_db(
            fixture.layout.chain_root.join("db"),
            DatabaseArguments::test(),
        )
        .unwrap();
        let tx = db.tx_mut().unwrap();
        match mutation {
            0 => {
                let mut header = fixture.headers[8].clone();
                header.inner.number = 80;
                tx.put::<tables::CanonicalHeaders>(8, header.hash_slow())
                    .unwrap();
                tx.put::<tables::Headers<OutbeHeader>>(8, header).unwrap();
            }
            1 => tx
                .put::<tables::CanonicalHeaders>(8, B256::repeat_byte(99))
                .unwrap(),
            _ => {
                tx.delete::<tables::CanonicalHeaders>(8, None).unwrap();
            }
        }
        tx.commit().unwrap();
        drop(db);
        let error = fixture.audit(&[]).unwrap_err();
        assert!(error.to_string().contains('8'), "{error:#}");
    }
}

#[test]
fn duplicate_mdbx_static_rows_must_agree_and_count_once() {
    for mutation in 0..3 {
        let fixture = Fixture::new();
        let db = init_db(
            fixture.layout.chain_root.join("db"),
            DatabaseArguments::test(),
        )
        .unwrap();
        let tx = db.tx_mut().unwrap();
        let mut header = fixture.headers[5].clone();
        if mutation == 1 {
            header.inner.extra_data = vec![99].into();
        }
        let hash = if mutation == 2 {
            B256::repeat_byte(99)
        } else {
            header.hash_slow()
        };
        tx.put::<tables::CanonicalHeaders>(5, hash).unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(5, header).unwrap();
        tx.commit().unwrap();
        drop(db);
        let result = fixture.audit(&[]);
        if mutation == 0 {
            let audit = result.unwrap();
            assert_eq!(audit.intervals, vec![4..=9]);
            assert_eq!(audit.verified_headers, 6);
        } else {
            let error = result.unwrap_err();
            assert!(error.to_string().contains('5'), "{error:#}");
        }
    }
}

#[test]
fn absent_mdbx_header_and_canonical_pair_is_reported_as_a_gap() {
    let fixture = Fixture::new();
    let db = init_db(
        fixture.layout.chain_root.join("db"),
        DatabaseArguments::test(),
    )
    .unwrap();
    let tx = db.tx_mut().unwrap();
    tx.delete::<tables::Headers<OutbeHeader>>(8, None).unwrap();
    tx.delete::<tables::CanonicalHeaders>(8, None).unwrap();
    tx.commit().unwrap();
    drop(db);
    let audit = fixture.audit(&[8, 9]).unwrap();
    assert_eq!(audit.intervals, vec![4..=7, 9..=9]);
    assert_eq!(audit.required_missing, vec![8]);
}

#[test]
fn available_genesis_identity_must_match_configured_chain() {
    for corrupt in [false, true] {
        let fixture = Fixture::new();
        let db = init_db(
            fixture.layout.chain_root.join("db"),
            DatabaseArguments::test(),
        )
        .unwrap();
        let tx = db.tx_mut().unwrap();
        let mut genesis = fixture.headers[0].clone();
        if corrupt {
            genesis.inner.timestamp += 1;
        }
        tx.put::<tables::CanonicalHeaders>(0, genesis.hash_slow())
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(0, genesis).unwrap();
        tx.commit().unwrap();
        drop(db);
        let result = fixture.audit(&[0]);
        if corrupt {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("genesis"), "{error:#}");
        } else {
            let audit = result.unwrap();
            assert_eq!(audit.intervals, vec![0..=0, 4..=9]);
            assert!(audit.required_missing.is_empty());
        }
    }
}
