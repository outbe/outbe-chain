use super::*;
use alloy_primitives::U256;

fn store_checkpoint(guard: &UpgradeJournalGuardV1, state: UpgradeJournalStateV1) {
    guard.store(UpgradeJournalSnapshotV1::new(state)).unwrap();
}

fn store_candidate_prepared(guard: &UpgradeJournalGuardV1, context: &UpgradeContextV1) {
    store_checkpoint(
        guard,
        UpgradeJournalStateV1::CandidatePrepared {
            context: context.clone(),
        },
    );
}

fn key_provisioned(context: UpgradeContextV1, sealed_root_hash: B256) -> UpgradeJournalStateV1 {
    UpgradeJournalStateV1::KeyProvisioned {
        context,
        sealed_root_hash,
    }
}

fn key_ready(context: UpgradeContextV1, sealed_root_hash: B256) -> UpgradeJournalStateV1 {
    UpgradeJournalStateV1::CandidateKeyReady {
        context,
        security: UpgradeSecurityMaterialV1 {
            sealed_root_hash,
            resident_offer_public: B256::repeat_byte(7),
            proof_hash: B256::repeat_byte(8),
        },
    }
}

pub(super) fn context(root: &Path) -> UpgradeContextV1 {
    UpgradeContextV1 {
        predecessor_manifest_hash: B256::repeat_byte(1),
        candidate_manifest_hash: B256::repeat_byte(2),
        successor_policy_hash: B256::repeat_byte(3),
        activation_height: 100,
        active_tee_dir: root.join("active"),
        candidate_tee_dir: root.join("candidate"),
    }
}

pub(super) fn submission() -> PreparedUpgradeSubmissionV1 {
    let calldata = vec![1, 2];
    let raw_transaction = vec![3, 4];
    PreparedUpgradeSubmissionV1 {
        intent_hash: B256::repeat_byte(3),
        evidence_hash: B256::repeat_byte(4),
        calldata_hash: keccak256(&calldata),
        relay: Address::repeat_byte(5),
        relay_variants: vec![RawRelayTransactionV1 {
            relay: Address::repeat_byte(5),
            chain_id: 1,
            account_nonce: 2,
            gas_price: U256::from(3),
            gas_limit: 4,
            calldata_hash: keccak256(&calldata),
            transaction_hash: keccak256(&raw_transaction),
            raw_transaction,
        }],
    }
}

#[test]
fn journal_roundtrips_all_security_relevant_checkpoints() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let guard = UpgradeJournalGuardV1::acquire(root.path()).unwrap();
    store_candidate_prepared(&guard, &context);
    store_checkpoint(
        &guard,
        key_provisioned(context.clone(), B256::repeat_byte(6)),
    );
    store_checkpoint(&guard, key_ready(context.clone(), B256::repeat_byte(6)));
    store_checkpoint(
        &guard,
        UpgradeJournalStateV1::SubmissionPrepared {
            context: context.clone(),
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash: B256::repeat_byte(6),
                resident_offer_public: B256::repeat_byte(7),
                proof_hash: B256::repeat_byte(8),
            },
            submission: submission(),
        },
    );
    store_checkpoint(
        &guard,
        UpgradeJournalStateV1::Submitted {
            context: context.clone(),
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash: B256::repeat_byte(6),
                resident_offer_public: B256::repeat_byte(7),
                proof_hash: B256::repeat_byte(8),
            },
            submission: submission(),
            submitted_at_finalized_height: 90,
            transaction_hashes: vec![submission().relay_variants[0].transaction_hash],
        },
    );
    store_checkpoint(
        &guard,
        UpgradeJournalStateV1::Finalized {
            context: context.clone(),
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash: B256::repeat_byte(6),
                resident_offer_public: B256::repeat_byte(7),
                proof_hash: B256::repeat_byte(8),
            },
            submission: submission(),
            finalized_height: 91,
            finalized_hash: B256::repeat_byte(9),
        },
    );
    store_checkpoint(
        &guard,
        UpgradeJournalStateV1::Promoted {
            context,
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash: B256::repeat_byte(6),
                resident_offer_public: B256::repeat_byte(7),
                proof_hash: B256::repeat_byte(8),
            },
            submission: submission(),
            finalized_height: 91,
            finalized_hash: B256::repeat_byte(9),
        },
    );
    let snapshot = guard.load().unwrap().unwrap();
    assert_eq!(snapshot.generation, 7);
    assert_eq!(snapshot.lifecycle.label(), "promoted");
    let metadata = fs::metadata(root.path().join(DIRECTORY).join("journal.json")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, FILE_MODE);
}

#[test]
fn journal_rejects_context_change_and_corrupt_committed_state() {
    let root = tempfile::tempdir().unwrap();
    let guard = UpgradeJournalGuardV1::acquire(root.path()).unwrap();
    let context = context(root.path());
    store_candidate_prepared(&guard, &context);
    let mut different = context;
    different.candidate_manifest_hash = B256::repeat_byte(9);
    assert!(guard
        .store(UpgradeJournalSnapshotV1::new(
            UpgradeJournalStateV1::CandidatePrepared { context: different },
        ))
        .is_err());
    drop(guard);
    fs::write(root.path().join(DIRECTORY).join("journal.json"), b"corrupt").unwrap();
    assert!(inspect_upgrade_journal_v1(root.path()).is_err());
}
