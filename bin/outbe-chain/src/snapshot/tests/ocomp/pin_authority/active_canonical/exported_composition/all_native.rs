use super::*;
use crate::snapshot::tests::native_state;
use crate::snapshot::{
    config::{
        parse_node_inputs, resolve_layout, resolve_requested_layout, NativeLayout, RequestedLayout,
    },
    tests::{evm::state_fixture, headers::fingerprint},
    validation::{
        report::{CheckName, CheckStatus, ValidationReport},
        run::{validate_snapshot, ValidationInputs},
    },
};
use alloy_consensus::Header;
use alloy_primitives::Address;
use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::projection::ProjectionCheckpoint;
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    models::StoredBlockBodyIndices,
    table::Table,
    tables::{self, ChainStateKey},
    transaction::{DbTx, DbTxMut},
};
use reth_primitives_traits::{Account, StorageEntry};
use std::ffi::OsString;

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
    ]
}

impl AllFixture {
    fn new(version: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let (source, _, _) = state_fixture(version);
        let inputs = parse_node_inputs(arguments(source.path())).unwrap();
        let layout = resolve_layout(&inputs).unwrap();
        let requested = resolve_requested_layout(&inputs).unwrap();
        let request = seal_source(&layout);
        let prepared = fixture_for_identity(
            &request,
            Phase::VotingOpen,
            layout.chain.chain().id(),
            layout.chain.genesis_hash(),
            bind_source,
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
        // This production create path:
        // 1. observes current native progress.
        // 2. closes readers.
        // 3. enumerates and hashes the actual damaged files.
        // 4. signs NEW manifest bytes.
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
    OutbeHeader::new(Header {
        number: 1,
        parent_hash: layout.chain.genesis_hash(),
        timestamp: 1_000,
        state_root: native_state::root(&source_state(layout.chain.chain().id())),
        ..Default::default()
    })
}

fn seed_execution(
    layout: &NativeLayout,
    version: u32,
    request: &OutbeHeader,
    owner: HashMapStorageProvider,
) {
    let before = source_state(layout.chain.chain().id());
    assert_eq!(native_state::root(&before), request.inner.state_root);
    let mut after = before.clone();
    native_state::merge_owner(&mut after, &owner);
    after.insert(
        ACCOUNT,
        (
            Account {
                nonce: 7,
                balance: U256::from(900),
                bytecode_hash: None,
            },
            Default::default(),
        ),
    );
    // Change a source ciphertext word at E. Only native history can prove B.
    let slot = source_ciphertext_slot(layout.chain.chain().id());
    let value = before[&TRIBUTE_ADDRESS].1[&slot];
    after
        .get_mut(&TRIBUTE_ADDRESS)
        .unwrap()
        .1
        .insert(slot, value ^ U256::ONE);
    native_state::persist(layout, version, &before, &after, 2);
    let root = native_state::root(&after);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    tx.clear::<tables::Headers<OutbeHeader>>().unwrap();
    tx.clear::<tables::CanonicalHeaders>().unwrap();
    tx.clear::<tables::HeaderNumbers>().unwrap();
    tx.clear::<tables::BlockBodyIndices>().unwrap();
    let mut parent = B256::ZERO;
    for number in 0..=400 {
        let header = if number == 0 {
            layout.chain.genesis_header().clone()
        } else if number == 1 {
            request.clone()
        } else {
            OutbeHeader::new(Header {
                number,
                parent_hash: parent,
                timestamp: 1_000 + number,
                state_root: root,
                ..Default::default()
            })
        };
        let hash = header.hash_slow();
        tx.put::<tables::CanonicalHeaders>(number, hash).unwrap();
        tx.put::<tables::HeaderNumbers>(hash, number).unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(number, header)
            .unwrap();
        tx.put::<tables::BlockBodyIndices>(
            number,
            StoredBlockBodyIndices {
                first_tx_num: 0,
                tx_count: 0,
            },
        )
        .unwrap();
        parent = hash;
    }
    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 1)
        .unwrap();
    for stage in ["Execution", "Finish"] {
        tx.put::<tables::StageCheckpoints>(stage.into(), StageCheckpoint::new(400))
            .unwrap();
    }
    tx.commit().unwrap();
}

fn write_frontiers(layout: &RequestedLayout, request: &OutbeHeader) {
    let point = ProjectionCheckpoint {
        block_number: 1,
        block_hash: request.hash_slow(),
    };
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
    for check in [CheckName::Headers, CheckName::Evm, CheckName::Ocomp] {
        assert_eq!(
            report.check(check).status,
            CheckStatus::Passed,
            "{check:?}: {report:?}"
        );
    }
    assert_eq!(report.observed.h.as_ref().unwrap().number, 1);
    assert_eq!(report.observed.e.as_ref().unwrap().number, 400);
    assert_eq!(report.observed.c_current.as_ref().unwrap().number, 1);
    assert!(report.required_missing.is_empty());
    assert_eq!(report.active_ocomp.len(), 1);
    assert!(report.active_ocomp[0].export_verified);
    for name in ["verified_source_leases", "verified_complete_exports"] {
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
fn all_native_checks_encrypted_state_and_active_export_at_independent_frontiers() {
    for version in [1, 2] {
        let fixture = AllFixture::new(version);
        let report = fixture.validate(&ValidationInputs::default());
        assert_native_success(&report);
        for check in [CheckName::Files, CheckName::Provenance] {
            assert_eq!(report.check(check).status, CheckStatus::NotRequested);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Damage {
    None,
    Evm,
    MissingHistory,
    WrongHistory,
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
        Damage::MissingHistory | Damage::WrongHistory => {
            let before = source_state(fixture.layout.chain.chain().id());
            let key = source_ciphertext_slot(fixture.layout.chain.chain().id());
            let value = before[&TRIBUTE_ADDRESS].1[&key];
            let db = init_db(
                fixture.layout.chain_root.join("db"),
                DatabaseArguments::test(),
            )
            .unwrap();
            let tx = db.tx_mut().unwrap();
            assert!(tx
                .delete::<tables::StorageChangeSets>((2, TRIBUTE_ADDRESS).into(), None)
                .unwrap());
            if matches!(damage, Damage::WrongHistory) {
                tx.put::<tables::StorageChangeSets>(
                    (2, TRIBUTE_ADDRESS).into(),
                    StorageEntry {
                        key,
                        value: value ^ U256::ONE,
                    },
                )
                .unwrap();
            }
            tx.commit().unwrap();
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

/// A freshly signed `all` artifact keeps files, provenance and headers passing and
/// reports `fault` in the native check it damages.
fn assert_damage_is_distinguished(fault: Damage) {
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
            return;
        }
        Damage::Evm => (CheckName::Evm, CheckStatus::Failed),
        Damage::MissingHistory | Damage::WrongHistory => (CheckName::Ocomp, CheckStatus::Failed),
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

#[test]
fn freshly_signed_all_passes_undamaged_native_state() {
    assert_damage_is_distinguished(Damage::None);
}

#[test]
fn freshly_signed_all_distinguishes_evm_damage_from_files_and_provenance() {
    assert_damage_is_distinguished(Damage::Evm);
}

#[test]
fn freshly_signed_all_rejects_missing_native_history_despite_valid_files_and_provenance() {
    assert_damage_is_distinguished(Damage::MissingHistory);
}

#[test]
fn freshly_signed_all_rejects_wrong_before_values_despite_valid_current_state() {
    assert_damage_is_distinguished(Damage::WrongHistory);
}

#[test]
fn freshly_signed_all_distinguishes_receipt_damage_from_files_and_provenance() {
    assert_damage_is_distinguished(Damage::Receipt);
}

#[test]
fn freshly_signed_all_distinguishes_missing_catalog_damage_from_files_and_provenance() {
    assert_damage_is_distinguished(Damage::MissingCatalog);
}
