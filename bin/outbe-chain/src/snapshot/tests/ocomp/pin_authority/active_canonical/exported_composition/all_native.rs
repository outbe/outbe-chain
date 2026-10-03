use super::*;
use crate::snapshot::tests::projection_fixture::PartitionFixtureStore as RocksDbStorage;
use crate::snapshot::{
    config::{
        parse_node_inputs, resolve_layout, resolve_requested_layout, NativeLayout,
        NativeReadSelection, RequestedLayout,
    },
    native::ce_identity,
    tests::{evm::state_fixture, headers::fingerprint},
    validation::{
        report::{CheckName, CheckStatus, ValidationReport},
        run::{validate_snapshot, ValidationInputs},
    },
};
use alloy_consensus::Header;
use alloy_primitives::Address;
use outbe_compressed_entities::{
    body_commitment, sealed_root, AuthenticatedParentTree, CeMdbx, EntityRef, ExactParentIdentity,
    FinalLeafMutation, FinalizedMarker, MdbxAuthenticatedTree, ACTIVE_COMMITMENT_SCHEME,
    BODY_SCHEMA_V1,
};
use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
use outbe_offchain_data::{ProjectionCheckpoint, ProjectionState, STORAGE_SCHEMA_VERSION};
use outbe_offchain_storage::{Key, Namespace, StorageWriter, Value};
use outbe_primitives::reshare_artifact::{
    encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
};
use reth_ethereum::provider::db::{
    cursor::DbCursorRO,
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    models::StoredBlockBodyIndices,
    table::Table,
    tables::{self, ChainStateKey},
    transaction::{DbTx, DbTxMut},
    DatabaseEnv, DatabaseEnvKind,
};
use reth_ethereum::trie::root::{state_root_unhashed, storage_root_unhashed};
use reth_primitives_traits::{Account, StorageEntry};
use std::{collections::BTreeMap, ffi::OsString, sync::Arc};

type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;
const ACCOUNT: Address = Address::repeat_byte(0xf7);

struct AllFixture {
    source: tempfile::TempDir,
    layout: NativeLayout,
    job: B256,
}

fn arguments(root: &Path) -> Vec<OsString> {
    vec![
        "--chain".into(),
        root.join("genesis.json").into_os_string(),
        "--datadir".into(),
        root.join("chain").into_os_string(),
        "--projection.storage-config".into(),
        root.join("configuration/offchain.toml").into_os_string(),
    ]
}

impl AllFixture {
    fn new(version: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let (source, _, _) = state_fixture(version);
        let config = source.path().join("configuration/offchain.toml");
        fs::write(
            &config,
            fs::read_to_string(&config)
                .unwrap()
                .replace("start_block = 17", "start_block = 0"),
        )
        .unwrap();
        let inputs = parse_node_inputs(arguments(source.path())).unwrap();
        let layout = resolve_layout(&inputs).unwrap();
        let requested =
            resolve_requested_layout(&inputs, NativeReadSelection { projection: true }).unwrap();
        write_source(&requested);
        let request = seal_source(&layout);
        // The root is read from the real committed CE marker, not a fixture constant.
        let marker = outbe_compressed_entities::CeMdbxReadOnly::open(
            &layout.chain_root,
            ce_identity(&layout),
        )
        .unwrap()
        .marker()
        .unwrap();
        let prepared = fixture_for_identity(
            &request,
            Phase::VotingOpen,
            layout.chain.chain().id(),
            layout.chain.genesis_hash(),
            |intent| {
                bind_source(intent);
                intent.ce_sealed_root = marker.new_root;
            },
        );
        let export = write_export(&layout.ocomp_root, &prepared);
        write_exported_pin(
            &layout.consensus_root.join("ocomp_retention"),
            &request,
            &prepared.job,
            export,
        );
        let job = prepared.job.finalized.as_ref().unwrap().job_id;
        seed_execution(&layout, version, &request, prepared.owner);
        write_frontiers(&requested, &request);
        fs::write(
            source.path().join("snapshot-signing-key.hex"),
            hex::encode([1u8; 32]),
        )
        .unwrap();
        fs::set_permissions(
            source.path().join("snapshot-signing-key.hex"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        // state_fixture already installs a protected recipient key and config.
        // Every writer and native read handle is closed before returning.
        Self {
            source,
            layout,
            job,
        }
    }

    fn validate(&self, inputs: &ValidationInputs) -> ValidationReport {
        let before = fingerprint(self.source.path());
        let scratch = tempfile::tempdir().unwrap();
        let scratch_before = fingerprint(scratch.path());
        let report =
            validate_snapshot(inputs, arguments(self.source.path()), scratch.path()).unwrap();
        assert_eq!(
            fingerprint(self.source.path()),
            before,
            "native source was changed"
        );
        assert_eq!(
            fingerprint(scratch.path()),
            scratch_before,
            "scratch leaked"
        );
        report
    }

    fn signed(&self, artifact: &Path) -> ValidationInputs {
        let before = fingerprint(self.source.path());
        let archive = artifact.join("snapshot.tar");
        // This production create path observes current native progress, closes readers,
        // enumerates and hashes the actual damaged files, then signs NEW manifest bytes.
        let (_, signer) = crate::snapshot::create::create(
            &archive,
            &self.source.path().join("snapshot-signing-key.hex"),
            Some("all-native fixture".into()),
            None,
            arguments(self.source.path()),
        )
        .unwrap();
        assert_eq!(
            fingerprint(self.source.path()),
            before,
            "create changed native source"
        );
        ValidationInputs {
            archive: Some(archive),
            expected_signer: Some(signer),
            ..Default::default()
        }
    }
}

fn seal_source(layout: &NativeLayout) -> OutbeHeader {
    // Use the same supported test precreation as tests/bodies.rs; do not alter owners.
    drop(
        reth_ethereum::provider::db::create_db(
            layout.chain_root.join("compressed_entities/smt"),
            DatabaseArguments::test(),
        )
        .unwrap(),
    );
    let genesis = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: layout.chain.genesis_hash(),
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    };
    let db = Arc::new(CeMdbx::open(&layout.chain_root, ce_identity(layout), genesis).unwrap());
    let parent = MdbxAuthenticatedTree::open(
        db.clone(),
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis.block_hash,
            root: genesis.new_root,
        },
    )
    .unwrap();
    let body = source_body();
    let bytes = encode_tribute_v1(&outbe_tribute::canonical_body(&body)).unwrap();
    let leaf = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        body.tribute_id,
        &bytes,
    )
    .unwrap();
    let seal = parent
        .prepare_seal(
            1,
            &[FinalLeafMutation {
                entity: EntityRef::Tribute(body.tribute_id),
                final_leaf: Some(leaf),
            }],
            &[],
        )
        .unwrap();
    let request = OutbeHeader::new(Header {
        number: 1,
        parent_hash: genesis.block_hash,
        timestamp: 1_000,
        extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                r_sealed: seal.new_root(),
            }),
            ..Default::default()
        })
        .unwrap(),
        ..Default::default()
    });
    db.apply_finalized(&seal.freeze(request.hash_slow()))
        .unwrap();
    request
}

fn seed_execution(
    layout: &NativeLayout,
    version: u32,
    request: &OutbeHeader,
    owner: HashMapStorageProvider,
) {
    let mut accounts: BTreeMap<Address, (Account, Vec<(B256, U256)>)> = BTreeMap::new();
    accounts.insert(
        ACCOUNT,
        (
            Account {
                nonce: 7,
                balance: U256::from(900),
                bytecode_hash: None,
            },
            vec![],
        ),
    );
    for ((address, slot), value) in owner.storage {
        if !value.is_zero() {
            accounts
                .entry(address)
                .or_default()
                .1
                .push((B256::from(slot.to_be_bytes::<32>()), value));
        }
    }
    let state_root = state_root_unhashed(accounts.iter().map(|(address, (account, words))| {
        (
            *address,
            (*account).into_trie_account(storage_root_unhashed(words.iter().copied())),
        )
    }));
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    tx.clear::<tables::PlainAccountState>().unwrap();
    tx.clear::<tables::PlainStorageState>().unwrap();
    tx.clear::<tables::HashedAccounts>().unwrap();
    tx.clear::<tables::HashedStorages>().unwrap();
    tx.clear::<tables::Headers<OutbeHeader>>().unwrap();
    tx.clear::<tables::CanonicalHeaders>().unwrap();
    for (address, (account, words)) in accounts {
        if version == 1 {
            tx.put::<tables::PlainAccountState>(address, account)
                .unwrap();
            for (key, value) in words {
                tx.put::<tables::PlainStorageState>(address, StorageEntry { key, value })
                    .unwrap();
            }
        } else {
            tx.put::<tables::HashedAccounts>(keccak256(address), account)
                .unwrap();
            for (key, value) in words {
                tx.put::<tables::HashedStorages>(
                    keccak256(address),
                    StorageEntry {
                        key: keccak256(key),
                        value,
                    },
                )
                .unwrap();
            }
        }
    }
    let execution = OutbeHeader::new(Header {
        number: 400,
        parent_hash: request.hash_slow(),
        timestamp: 1_010,
        state_root,
        ..Default::default()
    });
    for header in [
        layout.chain.genesis_header().clone(),
        request.clone(),
        execution,
    ] {
        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
            .unwrap();
    }
    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 1)
        .unwrap();
    for stage in ["Execution", "Finish"] {
        tx.put::<tables::StageCheckpoints>(stage.into(), StageCheckpoint::new(400))
            .unwrap();
    }
    tx.put::<tables::BlockBodyIndices>(
        1,
        StoredBlockBodyIndices {
            first_tx_num: 0,
            tx_count: 0,
        },
    )
    .unwrap();
    tx.clear::<tables::AccountChangeSets>().unwrap();
    tx.clear::<tables::StorageChangeSets>().unwrap();
    tx.commit().unwrap();
}

fn write_frontiers(layout: &RequestedLayout, request: &OutbeHeader) {
    let point = ProjectionCheckpoint {
        block_number: 1,
        block_hash: request.hash_slow(),
    };
    let location = layout.projection.as_ref().unwrap();
    let projection = RocksDbStorage::open(&location.root).unwrap();
    let state = ProjectionState {
        chain_id: layout.chain.chain().id(),
        genesis_hash: layout.chain.genesis_hash(),
        storage_schema_version: STORAGE_SCHEMA_VERSION,
        start_block: location.start_block,
        checkpoint: Some(point),
    };
    projection
        .put(
            Namespace::new("projection_state").unwrap(),
            &Key::new(b"offchain_data".to_vec()).unwrap(),
            &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
        )
        .unwrap();
    drop(projection);
    let baseline = ProjectionCheckpoint {
        block_number: 0,
        block_hash: layout.chain.genesis_hash(),
    };
    let closure = ContiguousCheckpointStoreV1::open(
        layout
            .ocomp_root
            .join("exporter-v1/discovery/closure-checkpoint-v1"),
        baseline,
    )
    .unwrap();
    closure.compare_and_advance_to(baseline, point).unwrap();
}

fn assert_native_success(report: &ValidationReport) {
    for check in [
        CheckName::Headers,
        CheckName::Evm,
        CheckName::Ce,
        CheckName::Bodies,
        CheckName::Ocomp,
    ] {
        assert_eq!(
            report.check(check).status,
            CheckStatus::Passed,
            "{check:?}: {report:?}"
        );
    }
    assert_eq!(report.observed.h.as_ref().unwrap().number, 1);
    assert_eq!(report.observed.e.as_ref().unwrap().number, 400);
    assert_eq!(report.observed.q.as_ref().unwrap().number, 1);
    assert_eq!(report.observed.p.as_ref().unwrap().number, 1);
    assert_eq!(report.observed.c_current.as_ref().unwrap().number, 1);
    assert!(report.required_missing.is_empty());
    assert_eq!(report.active_ocomp.len(), 1);
    assert!(report.active_ocomp[0].export_verified);
    for name in [
        "ce_leaves",
        "live_projection_bodies",
        "verified_source_leases",
        "verified_complete_exports",
    ] {
        assert!(
            report
                .inventory_bounds
                .iter()
                .any(|bound| bound.name == name && bound.visited > 0),
            "{name}: {report:?}"
        );
    }
    assert!(report.success(), "{report:?}");
}

#[test]
fn all_native_checks_real_nonzero_ce_body_and_active_export_at_independent_frontiers() {
    for version in [1, 2] {
        let fixture = AllFixture::new(version);
        let report = fixture.validate(&ValidationInputs::default());
        assert_native_success(&report);
        for check in [CheckName::Files, CheckName::Provenance] {
            assert_eq!(report.check(check).status, CheckStatus::NotRequested);
        }
    }
}

#[test]
fn unequal_body_frontiers_preserve_successful_structure_in_json() {
    for same_height in [false, true] {
        let fixture = AllFixture::new(2);
        let checkpoint = ProjectionCheckpoint {
            block_number: if same_height { 1 } else { 0 },
            block_hash: if same_height {
                B256::repeat_byte(0x99)
            } else {
                fixture.layout.chain.genesis_hash()
            },
        };
        let storage = RocksDbStorage::open(&fixture.layout.offchain_root).unwrap();
        let state = ProjectionState {
            chain_id: fixture.layout.chain.chain().id(),
            genesis_hash: fixture.layout.chain.genesis_hash(),
            storage_schema_version: STORAGE_SCHEMA_VERSION,
            start_block: fixture.layout.projection_start_block,
            checkpoint: Some(checkpoint),
        };
        storage
            .put(
                Namespace::new("projection_state").unwrap(),
                &Key::new(b"offchain_data".to_vec()).unwrap(),
                &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
            )
            .unwrap();
        drop(storage);
        let inputs = ValidationInputs {
            checks: "bodies".into(),
            ..Default::default()
        };
        let report = fixture.validate(&inputs);
        assert_eq!(report.check(CheckName::Ce).status, CheckStatus::Passed);
        assert_eq!(
            report.check(CheckName::Bodies).status,
            CheckStatus::Incomplete
        );
        assert!(!report.success());
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["body_structure"]["status"], "passed");
        assert_eq!(
            json["body_structure"]["checkpoint"]["number"],
            checkpoint.block_number
        );
        assert_eq!(
            json["body_structure"]["checkpoint"]["hash"],
            hex::encode(checkpoint.block_hash)
        );
        assert_eq!(json["observed"]["q"]["number"], 1);
        assert_eq!(json["observed"]["p"]["number"], checkpoint.block_number);
        assert!(!report
            .retained_ranges
            .iter()
            .any(|range| range.domain == "live_body_equality"));
        assert!(!report
            .inventory_bounds
            .iter()
            .any(|bound| bound.name == "live_projection_bodies"));

        // Even with unavailable equality, primary corruption must fail structure.
        let storage = RocksDbStorage::open(&fixture.layout.offchain_root).unwrap();
        storage
            .put(
                Namespace::new("nod_buckets").unwrap(),
                &Key::new(source_body().tribute_id.to_vec()).unwrap(),
                &Value::new(vec![0xff]).unwrap(),
            )
            .unwrap();
        drop(storage);
        let corrupt = fixture.validate(&inputs);
        assert_eq!(corrupt.check(CheckName::Ce).status, CheckStatus::Passed);
        assert_eq!(corrupt.check(CheckName::Bodies).status, CheckStatus::Failed);
        assert!(serde_json::to_value(&corrupt).unwrap()["body_structure"].is_null());
    }
}

#[test]
fn body_structure_observation_is_not_fabricated_when_unselected() {
    let fixture = AllFixture::new(2);
    let report = fixture.validate(&ValidationInputs {
        checks: "headers".into(),
        ..Default::default()
    });
    assert_eq!(
        report.check(CheckName::Bodies).status,
        CheckStatus::NotRequested
    );
    assert!(serde_json::to_value(&report).unwrap()["body_structure"].is_null());
}

#[derive(Clone, Copy, Debug)]
enum Damage {
    None,
    Evm,
    Ce,
    Body,
    Receipt,
    MissingCatalog,
}

fn damage(fixture: &AllFixture, damage: Damage) {
    match damage {
        Damage::None => {}
        Damage::Evm => {
            let db = init_db(
                fixture.layout.chain_root.join("db"),
                DatabaseArguments::test(),
            )
            .unwrap();
            let tx = db.tx_mut().unwrap();
            let mut account = tx
                .get::<tables::HashedAccounts>(keccak256(ACCOUNT))
                .unwrap()
                .unwrap();
            account.balance += U256::ONE;
            tx.put::<tables::HashedAccounts>(keccak256(ACCOUNT), account)
                .unwrap();
            tx.commit().unwrap();
        }
        Damage::Ce => {
            #[derive(Debug)]
            struct TestCeLeaves;
            impl Table for TestCeLeaves {
                const NAME: &'static str = "OutbeCompressedEntitiesLeavesV3";
                const DUPSORT: bool = false;
                type Key = Vec<u8>;
                type Value = Vec<u8>;
            }
            let db = DatabaseEnv::open(
                &fixture.layout.chain_root.join("compressed_entities/smt"),
                DatabaseEnvKind::RW,
                DatabaseArguments::test(),
            )
            .unwrap();
            let tx = db.tx_mut().unwrap();
            let (key, previous) = tx
                .cursor_read::<TestCeLeaves>()
                .unwrap()
                .seek(vec![1])
                .unwrap()
                .unwrap();
            assert_eq!(key[0], 1);
            let wrong = B256::with_last_byte(42).to_vec();
            assert_ne!(previous, wrong);
            tx.put::<TestCeLeaves>(key, wrong).unwrap();
            tx.commit().unwrap();
        }
        Damage::Body => {
            let storage = Arc::new(RocksDbStorage::open(&fixture.layout.offchain_root).unwrap());
            let mut body = source_body();
            body.nominal_amount_minor += U256::ONE;
            outbe_tribute::TributeRepositoryWriter::new(storage.clone(), storage)
                .put(&body)
                .unwrap();
        }
        Damage::Receipt => {
            let file = fixture
                .layout
                .ocomp_root
                .join("exporter-v1/receipts")
                .join(hex::encode(fixture.job))
                .join("receipt.ref");
            assert!(file.is_file());
            fs::write(file, b"malformed native receipt reference").unwrap();
        }
        Damage::MissingCatalog => {
            fs::remove_dir_all(
                fixture
                    .layout
                    .ocomp_root
                    .join("exporter-v1/input-refs")
                    .join(hex::encode(fixture.job)),
            )
            .unwrap();
        }
    }
}

#[test]
fn freshly_signed_all_distinguishes_native_semantic_damage_from_files_and_provenance() {
    for fault in [
        Damage::None,
        Damage::Evm,
        Damage::Ce,
        Damage::Body,
        Damage::Receipt,
        Damage::MissingCatalog,
    ] {
        let fixture = AllFixture::new(2);
        damage(&fixture, fault);
        let artifact = tempfile::tempdir().unwrap();
        let inputs = fixture.signed(artifact.path());
        let before = fingerprint(artifact.path());
        let report = fixture.validate(&inputs);
        assert_eq!(fingerprint(artifact.path()), before);
        for check in [CheckName::Files, CheckName::Provenance, CheckName::Headers] {
            assert_eq!(
                report.check(check).status,
                CheckStatus::Passed,
                "{fault:?}, {check:?}: {report:?}"
            );
        }
        assert_eq!(report.provenance.signature_valid, Some(true));
        assert_eq!(report.provenance.expected_signer_match, Some(true));
        let expected = match fault {
            Damage::None => {
                assert_native_success(&report);
                continue;
            }
            Damage::Evm => (CheckName::Evm, CheckStatus::Failed),
            Damage::Ce => (CheckName::Ce, CheckStatus::Failed),
            Damage::Body => (CheckName::Bodies, CheckStatus::Failed),
            Damage::Receipt => (CheckName::Ocomp, CheckStatus::Failed),
            Damage::MissingCatalog => (CheckName::Ocomp, CheckStatus::Incomplete),
        };
        assert_eq!(
            report.check(expected.0).status,
            expected.1,
            "{fault:?}: {report:?}"
        );
        assert!(report.check(expected.0).diagnostic.is_some());
        assert!(!report.success());
    }
}
