use super::*;
use crate::manager::{FinalizedBlock, ManagerError};
use outbe_primitives::radicle_publication::PublicationAction;
use std::collections::BTreeMap;
use std::sync::Mutex;

struct ReceiptSource {
    rows: BTreeMap<u64, FinalizedPublicationReceipt>,
    reads: Mutex<Vec<u64>>,
    unavailable: bool,
}

impl RepositoryReceiptReader for ReceiptSource {
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        self.reads.lock().unwrap().push(sequence);
        if self.unavailable {
            Err(ManagerError::Finality(
                "local finalized state unavailable".into(),
            ))
        } else {
            Ok(self.rows.get(&sequence).cloned())
        }
    }
}

// Canonical manifest/receipt commitments, not a complete native Git package or chain fixture.
fn checkpoint_chain() -> Vec<(FinalizedPublicationReceipt, PublicationManifest)> {
    let (mut body, initial) = super::super::tests::bootstrap_fixture();
    let mut rows = vec![];
    for sequence in 1..=4 {
        let manifest = if sequence == 1 {
            initial.clone()
        } else {
            body.action = PublicationAction::BindMaterial;
            body.epoch += 1;
            body.material_version += 1;
            body.material_hash = B256::repeat_byte(sequence as u8);
            PublicationManifest::new(vec![], vec![], vec![], initial.owner_snapshot().to_vec())
                .unwrap()
        };
        body.sequence = sequence;
        body.package_manifest_hash = manifest.hash().unwrap();
        let checkpoint = FinalizedPublicationReceipt {
            block: FinalizedBlock {
                number: 20,
                hash: B256::repeat_byte(20),
            },
            receipt: outbe_coderegistry::publication::PublicationReceipt {
                body: body.clone(),
                accepted_height: sequence,
                receipt_hash: body.receipt_hash(),
            },
        };
        body.previous_receipt_hash = checkpoint.receipt().receipt_hash;
        rows.push((checkpoint, manifest));
    }
    rows
}

fn receipt_source(rows: &[(FinalizedPublicationReceipt, PublicationManifest)]) -> ReceiptSource {
    ReceiptSource {
        rows: rows
            .iter()
            .map(|(r, _)| (r.receipt().body.sequence, r.clone()))
            .collect(),
        reads: Mutex::new(vec![]),
        unavailable: false,
    }
}

#[test]
fn accepted_manifest_cache_replays_exact_snapshot_after_reopen_without_selecting_package() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("archive");
    let rows = checkpoint_chain();
    let mut store = ReviewPackageStore::open(&root).unwrap();
    for index in [3, 1, 0, 2, 0] {
        store
            .persist_checkpoint_manifest(&rows[index].0, &rows[index].1)
            .unwrap();
    }
    let target = &rows[3].0;
    assert!(store
        .selection(review_slot_key(&target.receipt().body))
        .unwrap()
        .is_none());
    assert!(!store
        .package_dir(target.receipt().receipt_hash)
        .join("ready")
        .exists());
    let initial_bootstrap =
        CheckpointOwnerSnapshot::bootstrap(rows[0].0.clone(), &rows[0].1).unwrap();
    let expected_owner_refs = initial_bootstrap.owner_refs().clone();
    drop(store);
    let reopened = ReviewPackageStore::open(&root).unwrap();
    let source = receipt_source(&rows);
    let actual = reopened
        .replay_checkpoint_snapshot(&source, target)
        .unwrap();
    assert_eq!(actual.checkpoint(), target);
    assert_eq!(actual.owner_refs(), &expected_owner_refs);
    assert_eq!(
        actual.sigrefs().to_string(),
        "0303030303030303030303030303030303030303"
    );
    assert_eq!(*source.reads.lock().unwrap(), vec![1, 2, 3, 4]);
}

#[test]
fn checkpoint_replay_requires_trusted_receipts_and_rejects_gaps_forks_or_foreign_target() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = ReviewPackageStore::open(directory.path().join("archive")).unwrap();
    let rows = checkpoint_chain();
    for (receipt, manifest) in &rows {
        store
            .persist_checkpoint_manifest(receipt, manifest)
            .unwrap();
    }
    let target = &rows[3].0;
    let mut source = receipt_source(&rows);
    source.unavailable = true;
    assert!(matches!(
        store.replay_checkpoint_snapshot(&source, target),
        Err(Error::Authority(_))
    ));
    source.unavailable = false;
    let missing = source.rows.remove(&2).unwrap();
    assert!(matches!(
        store.replay_checkpoint_snapshot(&source, target),
        Err(Error::MissingCheckpoint(2))
    ));
    source.rows.insert(2, rows[2].0.clone());
    assert!(matches!(
        store.replay_checkpoint_snapshot(&source, target),
        Err(Error::Corrupt)
    ));
    let mut fork = missing;
    fork.receipt.body.previous_receipt_hash = B256::repeat_byte(99);
    fork.receipt.receipt_hash = fork.receipt.body.receipt_hash();
    store
        .persist_checkpoint_manifest(&fork, &rows[1].1)
        .unwrap();
    source.rows.insert(2, fork);
    assert!(store.replay_checkpoint_snapshot(&source, target).is_err());
    source = receipt_source(&rows);
    let mut wrong_target = target.clone();
    wrong_target.receipt.body.genesis_hash = B256::repeat_byte(99);
    wrong_target.receipt.receipt_hash = wrong_target.receipt.body.receipt_hash();
    assert!(store
        .replay_checkpoint_snapshot(&source, &wrong_target)
        .is_err());
    wrong_target.receipt.body.sequence = 0;
    assert!(store
        .replay_checkpoint_snapshot(&source, &wrong_target)
        .is_err());
}

#[test]
fn checkpoint_manifest_cache_rejects_corruption_missing_files_and_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = ReviewPackageStore::open(directory.path().join("archive")).unwrap();
    let rows = checkpoint_chain();
    for (receipt, manifest) in &rows {
        store
            .persist_checkpoint_manifest(receipt, manifest)
            .unwrap();
    }
    let checkpoint = &rows[1].0;
    let package = store.package_dir(checkpoint.receipt().receipt_hash);
    let manifest_path = package.join("manifest");
    let bytes = rows[1].1.to_bytes().unwrap();
    fs::remove_file(&manifest_path).unwrap();
    assert!(store
        .replay_checkpoint_snapshot(&receipt_source(&rows), &rows[3].0)
        .is_err());
    fs::write(&manifest_path, vec![0; MAX_MANIFEST_BYTES + 1]).unwrap();
    assert!(matches!(
        store.load_checkpoint_manifest(checkpoint),
        Err(Error::Corrupt)
    ));
    fs::write(&manifest_path, &bytes).unwrap();
    fs::write(package.join("body"), b"corrupt").unwrap();
    assert!(matches!(
        store.load_checkpoint_manifest(checkpoint),
        Err(Error::Corrupt)
    ));
    fs::write(package.join("body"), checkpoint.receipt().body.to_bytes()).unwrap();
    let mut wrong = rows[1].1.clone();
    wrong.owner_snapshot[0].oid = "99".repeat(20);
    fs::write(&manifest_path, wrong.to_bytes().unwrap()).unwrap();
    assert!(store.load_checkpoint_manifest(checkpoint).is_err());
    fs::write(&manifest_path, b"{}").unwrap();
    assert!(store.load_checkpoint_manifest(checkpoint).is_err());
    fs::remove_file(&manifest_path).unwrap();
    let outside = directory.path().join("untouched");
    fs::write(&outside, &bytes).unwrap();
    std::os::unix::fs::symlink(&outside, &manifest_path).unwrap();
    assert!(matches!(
        store.load_checkpoint_manifest(checkpoint),
        Err(Error::Corrupt)
    ));
    assert_eq!(fs::read(&outside).unwrap(), bytes);
    assert!(store
        .persist_checkpoint_manifest(checkpoint, &rows[0].1)
        .is_err());
    assert_eq!(fs::read(&outside).unwrap(), bytes);
}

#[test]
fn selected_slot_survives_reopen_and_cannot_choose_another_body() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("nested/store");
    let mut store = ReviewPackageStore::open(&root).unwrap();
    assert!(matches!(
        ReviewPackageStore::open(&root),
        Err(Error::Ownership(_))
    ));
    let slot = B256::repeat_byte(1);
    let body = B256::repeat_byte(2);
    let witness = B256::repeat_byte(3);
    store.select(slot, body, witness).unwrap();
    drop(store);
    let mut reopened = ReviewPackageStore::open(&root).unwrap();
    assert_eq!(reopened.selection(slot).unwrap(), Some((body, witness)));
    reopened.select(slot, body, B256::repeat_byte(4)).unwrap();
    assert!(matches!(
        reopened.select(slot, B256::repeat_byte(5), witness),
        Err(Error::SlotConflict)
    ));
    assert_eq!(reopened.selection(slot).unwrap(), Some((body, witness)));
    fs::write(root.join("slots").join(hex::encode(slot)), b"corrupt").unwrap();
    assert!(matches!(reopened.selection(slot), Err(Error::Corrupt)));
}

#[test]
fn atomic_artifacts_recover_uncommitted_temporary_bytes_without_overwriting_committed_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("artifact");
    fs::write(path.with_extension("pending"), b"partial").unwrap();
    write_once(&path, b"complete").unwrap();
    assert_eq!(read(&path, 8).unwrap(), b"complete");
    write_once(&path, b"complete").unwrap();
    assert!(matches!(
        write_once(&path, b"different"),
        Err(Error::Corrupt)
    ));
    assert_eq!(read(&path, 8).unwrap(), b"complete");
    assert!(matches!(read(&path, 7), Err(Error::Corrupt)));
    assert!(!path.with_extension("pending").exists());
}

#[test]
fn symlinks_and_public_store_directories_fail_without_changing_the_target() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target");
    fs::write(&target, b"preserve").unwrap();
    let path = directory.path().join("artifact");
    std::os::unix::fs::symlink(&target, path.with_extension("pending")).unwrap();
    assert!(matches!(write_once(&path, b"replace"), Err(Error::Corrupt)));
    assert_eq!(fs::read(&target).unwrap(), b"preserve");
    let public = directory.path().join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        ReviewPackageStore::open(&public),
        Err(Error::Corrupt)
    ));
    assert_eq!(
        fs::metadata(&public).unwrap().permissions().mode() & 0o777,
        0o755
    );
}
