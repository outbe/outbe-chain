//! Real offline validate CLI coverage using stopped native files and conventional placement.
//!
//! The shared task01 fixture
//! intentionally contains incomplete EVM progress and opaque unfinished OCOMP
//! bytes. Only prepare_evm_authority below establishes a valid current-E fixture;
//! these tests do not claim CE/body/OCOMP all-check acceptance or node startup.

use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use alloy_consensus::Sealable;
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::OutbeHeader;
use outbe_snapshot::{archive::read_archive_index, manifest::NativeRoot};
use reth_ethereum::{
    provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        table::Table,
        tables,
        transaction::{DbTx, DbTxMut},
    },
    trie::root::{state_root_unhashed, storage_root_unhashed},
};
use reth_primitives_traits::{Account, StorageEntry};
use serde_json::Value;

type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;

#[path = "common/snapshot.rs"]
mod snapshot;
use snapshot::{binary, fingerprint, run, stopped_fixture, transcript, StoppedFixture};

const OTHER_SIGNER: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

fn successful(command: &mut Command) -> Output {
    let output = run(command);
    assert!(output.status.success(), "{}", transcript(&output));
    output
}

fn read_report(path: &Path) -> Value {
    let json: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(json.get("protected_paths").is_none());
    json
}

fn status<'a>(report: &'a Value, check: &str) -> &'a str {
    report["checks"][check]["status"].as_str().unwrap()
}

fn native_arguments(command: &mut Command, fixture: &StoppedFixture) {
    command
        .args(["--", "--chain"])
        .arg(&fixture.genesis)
        .arg("--datadir")
        .arg(&fixture.chain)
        .arg("--projection.storage-config")
        .arg(&fixture.projection_config);
}

fn create_archive(fixture: &StoppedFixture, archive: &Path) {
    let before = fingerprint(&fixture.donor);
    let mut command = binary();
    command
        .args(["snapshot", "create", "--output"])
        .arg(archive)
        .arg("--signing-key")
        .arg(&fixture.signing_key);
    native_arguments(&mut command, fixture);
    successful(&mut command);
    assert_eq!(fingerprint(&fixture.donor), before);
}

// Match the existing snapshot/tests/evm.rs current-state fixture. This setup is
// local to the integration test: no historical replay, fake validator, or runtime
// behavior is introduced. State is nonempty and authoritative v2 tables are used.
fn prepare_evm_authority(fixture: &mut StoppedFixture, corrupt_after_binding: bool) -> B256 {
    let address = Address::repeat_byte(0x11);
    let slot = B256::repeat_byte(0x22);
    let value = U256::from(123);
    let account = Account {
        nonce: 7,
        balance: U256::from(900),
        bytecode_hash: None,
    };
    let expected = state_root_unhashed([(
        address,
        account.into_trie_account(storage_root_unhashed([(slot, value)])),
    )]);
    let db = init_db(fixture.chain.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    let genesis = tx.get::<tables::Headers<OutbeHeader>>(0).unwrap().unwrap();
    assert_eq!(genesis.hash_slow(), fixture.genesis_hash);
    tx.put::<tables::HashedAccounts>(keccak256(address), account)
        .unwrap();
    tx.put::<tables::HashedStorages>(
        keccak256(address),
        StorageEntry {
            key: keccak256(slot),
            value,
        },
    )
    .unwrap();
    let mut execution = tx
        .get::<tables::Headers<OutbeHeader>>(101)
        .unwrap()
        .unwrap();
    execution.inner.state_root = expected;
    fixture.execution_hash = execution.hash_slow();
    tx.put::<tables::Headers<OutbeHeader>>(101, execution)
        .unwrap();
    tx.put::<tables::CanonicalHeaders>(101, fixture.execution_hash)
        .unwrap();
    for stage in ["Execution", "Finish"] {
        tx.put::<tables::StageCheckpoints>(stage.into(), StageCheckpoint::new(101))
            .unwrap();
    }
    tx.delete::<tables::Metadata>("partial_state_trie_unwind".into(), None)
        .unwrap();
    // The archive below will be freshly generated and signed AFTER corruption:
    // file equality and valid provenance must not override this stale state root.
    if corrupt_after_binding {
        tx.put::<tables::HashedAccounts>(
            keccak256(address),
            Account {
                balance: U256::from(901),
                ..account
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    expected
}

#[test]
fn validate_help_is_available_without_node_startup_and_restore_is_not_a_command() {
    let help = successful(binary().args(["snapshot", "validate", "--help"]));
    let text = transcript(&help);
    for option in [
        "--checks",
        "--manifest",
        "--signature",
        "--archive",
        "--expected-signer",
        "--report",
    ] {
        assert!(text.contains(option), "{text}");
    }
    let help = successful(binary().args(["snapshot", "--help"]));
    assert!(transcript(&help).contains("validate"));
    assert!(!transcript(&help).contains("restore"));
    let restore = run(binary().args(["snapshot", "restore"]));
    assert!(!restore.status.success());
}

#[test]
fn missing_artifact_reports_on_stdout_when_report_roots_cannot_be_resolved() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("missing.json");
    let output = run(binary()
        .args([
            "snapshot",
            "validate",
            "--checks",
            "provenance",
            "--expected-signer",
            OTHER_SIGNER,
            "--report",
        ])
        .arg(&target)
        .args(["--", "--intentionally-invalid-native-option"]));
    assert!(!output.status.success(), "{}", transcript(&output));
    assert!(!target.exists());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status(&report, "provenance"), "incomplete");
    assert_eq!(status(&report, "headers"), "not_requested");
    assert_eq!(status(&report, "evm"), "not_requested");
    assert!(report["provenance"]["signature_valid"].is_null());
    assert!(report["provenance"]["expected_signer_match"].is_null());
    assert!(
        transcript(&output).contains("unexpected argument '--intentionally-invalid-native-option'")
    );
    let stdout_only = run(binary().args([
        "snapshot",
        "validate",
        "--checks",
        "provenance",
        "--",
        "--intentionally-invalid-native-option",
    ]));
    assert!(!stdout_only.status.success());
    let report: serde_json::Value = serde_json::from_slice(&stdout_only.stdout).unwrap();
    assert_eq!(status(&report, "provenance"), "incomplete");
    assert!(!transcript(&stdout_only).contains("unexpected argument"));
}

#[test]
fn report_output_preserves_existing_files_and_rejects_native_roots_even_with_bad_projection_config()
{
    let root = tempfile::tempdir().unwrap();
    let fixture = stopped_fixture(&root.path().join("donor"));
    let existing = root.path().join("existing-report.json");
    fs::write(&existing, b"operator-owned existing bytes").unwrap();
    let existing_output = run(binary()
        .args(["snapshot", "validate", "--checks", "provenance", "--report"])
        .arg(&existing));
    assert!(!existing_output.status.success());
    assert_eq!(
        fs::read(&existing).unwrap(),
        b"operator-owned existing bytes"
    );

    // The base ordinary layout remains knowable even when projection TOML fails.
    fs::write(&fixture.projection_config, b"not valid TOML = [").unwrap();
    let alias = root.path().join("chain-alias");
    symlink(&fixture.chain, &alias).unwrap();
    let before = fingerprint(&fixture.donor);
    for target in [
        fixture.chain.join("audit.json"),
        fixture.chain.join("keys/audit.json"),
        alias.join("aliased-audit.json"),
        fixture.signing_key.clone(),
        fixture.projection_config.clone(),
    ] {
        let mut command = binary();
        command
            .args(["snapshot", "validate", "--checks", "bodies", "--report"])
            .arg(&target);
        native_arguments(&mut command, &fixture);
        let output = run(&mut command);
        assert!(!output.status.success(), "{}", transcript(&output));
        assert_eq!(fingerprint(&fixture.donor), before);
    }
    let target = root.path().join("bad-projection.json");
    let mut command = binary();
    command
        .args(["snapshot", "validate", "--checks", "bodies", "--report"])
        .arg(&target);
    native_arguments(&mut command, &fixture);
    let output = run(&mut command);
    assert!(!output.status.success(), "{}", transcript(&output));
    assert!(!target.exists());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(status(&report, "bodies"), "passed");
    assert_eq!(fingerprint(&fixture.donor), before);
}

struct Placed {
    base: PathBuf,
    genesis: PathBuf,
    projection_config: PathBuf,
}

impl Placed {
    fn root(&self, kind: NativeRoot) -> PathBuf {
        match kind {
            NativeRoot::Chain => self.base.join("chain"),
            NativeRoot::Consensus => self.base.join("chain/consensus"),
            NativeRoot::Ocomp => self.base.join("ocomp/domain-v1"),
            NativeRoot::Offchain => self.base.join("projection"),
            NativeRoot::StaticFiles => self.base.join("chain/static_files"),
            NativeRoot::ExecutionRocksDb => self.base.join("chain/rocksdb"),
        }
    }
    fn native_arguments(&self, command: &mut Command) {
        command
            .args(["--", "--chain"])
            .arg(&self.genesis)
            .arg("--datadir")
            .arg(self.root(NativeRoot::Chain))
            .arg("--projection.storage-config")
            .arg(&self.projection_config);
    }
}

#[test]
fn signed_transfer_and_conventional_placement_do_not_mask_semantic_state_corruption() {
    for corrupt in [false, true] {
        let producer = tempfile::tempdir().unwrap();
        let mut fixture = stopped_fixture(&producer.path().join("donor"));
        let expected_root = prepare_evm_authority(&mut fixture, corrupt);
        assert_ne!(expected_root, B256::ZERO);
        let archive = producer.path().join("snapshot.tar");
        create_archive(&fixture, &archive);

        let receiver = tempfile::tempdir().unwrap();
        let incoming = receiver.path().join("incoming");
        fs::create_dir(&incoming).unwrap();
        let transferred = incoming.join("received.tar");
        successful(Command::new("cp").arg("--").arg(&archive).arg(&transferred));
        let index = read_archive_index(
            fs::File::open(&transferred).unwrap(),
            Some(&fixture.public_key),
        )
        .unwrap();
        let extracted = incoming.join("unpacked");
        fs::create_dir(&extracted).unwrap();
        successful(
            Command::new("tar")
                .args(["--no-same-owner", "-xpf"])
                .arg(&transferred)
                .arg("-C")
                .arg(&extracted),
        );
        assert_eq!(
            fs::read(extracted.join("manifest.json")).unwrap(),
            index.raw_manifest
        );
        let placed = Placed {
            base: receiver.path().join("placed"),
            genesis: receiver.path().join("placed/configuration/genesis.json"),
            projection_config: receiver.path().join("placed/configuration/offchain.toml"),
        };
        fs::create_dir_all(placed.genesis.parent().unwrap()).unwrap();
        fs::copy(&fixture.genesis, &placed.genesis).unwrap();
        fs::write(&placed.projection_config, "version = 1\nbackend = 'rocksdb'\nstart_block = 17\n[rocksdb]\npath = '../projection'\nsecondary_path = '../secondary'\n").unwrap();
        for domain in &index.manifest.domains {
            if domain.entries.is_empty() {
                continue;
            }
            let target = placed.root(domain.native_root);
            fs::create_dir_all(&target).unwrap();
            successful(
                Command::new("cp")
                    .args(["-a", "--no-preserve=ownership", "--"])
                    .arg(extracted.join("payload").join(&domain.id).join("."))
                    .arg(&target),
            );
            fs::set_permissions(&target, fs::Permissions::from_mode(domain.mode)).unwrap();
        }
        let recipient_key = placed.root(NativeRoot::Chain).join("keys/recipient.hex");
        fs::create_dir_all(recipient_key.parent().unwrap()).unwrap();
        fs::write(&recipient_key, b"independent recipient authority").unwrap();
        // Opaque task01 OCOMP input remains copied; no claim that OCOMP is valid.
        assert!(placed
            .root(NativeRoot::Ocomp)
            .join(&fixture.pending_result)
            .is_file());
        producer.close().unwrap();
        assert!(!fixture.donor.exists());
        assert!(!archive.exists());
        let before = fingerprint(&placed.base);

        let target = receiver.path().join("native-and-files.json");
        let mut command = binary();
        command
            .args(["snapshot", "validate", "--checks", "files,evm", "--archive"])
            .arg(&transferred)
            .arg("--expected-signer")
            .arg(hex::encode(fixture.public_key))
            .arg("--report")
            .arg(&target);
        placed.native_arguments(&mut command);
        let output = run(&mut command);
        assert_eq!(output.status.success(), !corrupt, "{}", transcript(&output));
        let report = read_report(&target);
        assert_eq!(status(&report, "files"), "passed", "{report:#}");
        assert_eq!(status(&report, "provenance"), "passed", "{report:#}");
        assert_eq!(status(&report, "headers"), "passed", "{report:#}");
        assert_eq!(
            status(&report, "evm"),
            if corrupt { "failed" } else { "passed" },
            "{report:#}"
        );
        for unselected in ["ce", "bodies", "ocomp"] {
            assert_eq!(status(&report, unselected), "not_requested");
        }
        assert_eq!(report["provenance"]["signature_valid"], true);
        assert_eq!(report["provenance"]["expected_signer_match"], true);
        assert_eq!(report["observed"]["h"]["number"], 100);
        assert_eq!(
            report["observed"]["h"]["hash"],
            hex::encode(fixture.finalized_hash)
        );
        assert_eq!(report["observed"]["e"]["number"], 101);
        assert_eq!(
            report["observed"]["e"]["hash"],
            hex::encode(fixture.execution_hash)
        );
        if corrupt {
            assert!(report["checks"]["evm"]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("state root differs"));
        }
        assert_eq!(fingerprint(&placed.base), before);
        assert_eq!(
            fs::read(&recipient_key).unwrap(),
            b"independent recipient authority"
        );

        // A stdout-only signature check needs no native layout and ignores
        // irrelevant native parser failures, independently of EVM corruption.
        let stdout_only = successful(
            binary()
                .args([
                    "snapshot",
                    "validate",
                    "--checks",
                    "provenance",
                    "--archive",
                ])
                .arg(&transferred)
                .args(["--", "--intentionally-invalid-native-option"]),
        );
        let stdout_report: Value = serde_json::from_slice(&stdout_only.stdout).unwrap();
        assert_eq!(status(&stdout_report, "provenance"), "passed");
        assert!(!transcript(&stdout_only).contains("unexpected argument"));

        // An artifact-only external report also needs no native configuration
        // when no native arguments were supplied. Invalid supplied arguments
        // prevent report publication; the dedicated report-path test covers it.
        let provenance_report = receiver.path().join("provenance-only.json");
        successful(
            binary()
                .args([
                    "snapshot",
                    "validate",
                    "--checks",
                    "provenance",
                    "--archive",
                ])
                .arg(&transferred)
                .arg("--report")
                .arg(&provenance_report),
        );
        assert_eq!(
            status(&read_report(&provenance_report), "provenance"),
            "passed"
        );

        let missing_report = receiver.path().join("missing-signature.json");
        let missing = run(binary()
            .args([
                "snapshot",
                "validate",
                "--checks",
                "provenance",
                "--manifest",
            ])
            .arg(extracted.join("manifest.json"))
            .arg("--expected-signer")
            .arg(hex::encode(fixture.public_key))
            .arg("--report")
            .arg(&missing_report));
        assert!(!missing.status.success(), "{}", transcript(&missing));
        assert_eq!(
            status(&read_report(&missing_report), "provenance"),
            "incomplete"
        );

        let wrong_report = receiver.path().join("wrong-signer.json");
        let wrong = run(binary()
            .args([
                "snapshot",
                "validate",
                "--checks",
                "provenance",
                "--archive",
            ])
            .arg(&transferred)
            .args(["--expected-signer", OTHER_SIGNER, "--report"])
            .arg(&wrong_report));
        assert!(!wrong.status.success(), "{}", transcript(&wrong));
        let wrong = read_report(&wrong_report);
        assert_eq!(status(&wrong, "provenance"), "failed");
        assert_eq!(wrong["provenance"]["signature_valid"], true);
        assert_eq!(wrong["provenance"]["expected_signer_match"], false);
        assert_eq!(fingerprint(&placed.base), before);
    }
}
