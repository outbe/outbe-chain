use super::*;
use std::os::unix::fs::{symlink, PermissionsExt as _};

#[test]
fn journal_rejects_tampered_signed_metadata_and_unknown_secret_fields() {
    for case in 0..5 {
        let (root, mut journal, _live, mut record, _) = fixture();
        match case {
            0 => record.tx_hash = B256::repeat_byte(99),
            1 => record.nonce += 1,
            2 => record.sender = Address::repeat_byte(99),
            3 => record.raw_signed_tx = vec![0x01; 32].into(),
            _ => record.finalized_checkpoint.chain_id += 1,
        }
        assert!(journal.begin(record).is_err());
        assert!(journal.load().unwrap().is_none());
        assert!(!root.path().join("tee-service-tx-v1/journal.json").exists());
    }
    let (root, mut journal, _live, record, _) = fixture();
    journal.begin(record).unwrap();
    let path = root.path().join("tee-service-tx-v1/journal.json");
    let mut wire: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    wire["record"]["seed"] = serde_json::json!(vec![0; 32]);
    std::fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
    assert!(journal.load().is_err());
}

#[test]
fn interrupted_scratch_never_replaces_committed_state_and_symlinks_are_rejected() {
    let (root, mut journal, _live, record, _) = fixture();
    journal.begin(record.clone()).unwrap();
    let directory = root.path().join("tee-service-tx-v1");
    let scratch = directory.join("journal.next");
    std::fs::write(&scratch, b"partial").unwrap();
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o600)).unwrap();
    drop(journal);
    let journal = ServiceTxJournalV1::open(root.path()).unwrap();
    assert_eq!(journal.load().unwrap(), Some(record));
    assert!(!scratch.exists());
    let outside = root.path().join("other");
    std::fs::write(&outside, b"unchanged").unwrap();
    symlink(&outside, &scratch).unwrap();
    drop(journal);
    assert!(ServiceTxJournalV1::open(root.path()).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
}

#[test]
fn pending_replacement_requires_same_account_nonce_and_new_process_identity() {
    let (root, mut journal, _live, original, _) = fixture();
    journal.begin(original.clone()).unwrap();
    let path = root.path().join("tee-service-tx-v1/journal.json");
    let before = std::fs::read(&path).unwrap();
    let (_, _, _, different_nonce, _) = fixture_with_identity_and_nonce(10, 5);
    let mut same_identity = original.clone();
    same_identity.public_intent.target.valid_until += 1;
    let (_, _, _, replacement, _) = fixture_with_identity_and_nonce(10, 4);
    let (_, _, _, different_sender, _) = fixture_for_account(10, 4, 7);
    let mut different_chain = replacement.clone();
    different_chain.finalized_checkpoint.genesis_hash = B256::repeat_byte(99);
    let mut old_signed_bytes = original.clone();
    old_signed_bytes.public_intent = replacement.public_intent.clone();
    for rejected in [
        different_nonce,
        same_identity,
        different_sender,
        different_chain,
        old_signed_bytes,
    ] {
        assert!(matches!(
            journal.begin(rejected),
            Err(ServiceTxJournalError::UnresolvedTransaction)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    assert_eq!(
        (replacement.sender, replacement.nonce),
        (original.sender, original.nonce)
    );
    assert_ne!(replacement.tx_hash, original.tx_hash);
    journal.begin(replacement.clone()).unwrap();
    assert_eq!(journal.load().unwrap(), Some(replacement.clone()));
    drop(journal);
    let journal = ServiceTxJournalV1::open(root.path()).unwrap();
    assert_eq!(journal.load().unwrap(), Some(replacement));
}
