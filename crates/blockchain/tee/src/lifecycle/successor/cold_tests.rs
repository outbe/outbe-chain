//! Cold recovery never needs the retired resident's keys.
use super::*;
use crate::lifecycle::renewal::tests::support::{self, Chain, Signer};
use crate::lifecycle::*;
use crate::registry_view::FinalizedStagedSuccessorPolicyV1;
use alloy_primitives::{B256, U256};
use outbe_primitives::enclave_upgrade::EnclaveUpgradeV1;

fn cold_view() -> LifecycleRegistryStateV1 {
    // The old context is dropped before the operation is invoked.
    let mut view = support::view(&support::context(8));
    view.checkpoint.height = 100;
    view.checkpoint.timestamp = 7200;
    view.schedule.finalized_height = 100;
    view.schedule.finalized_timestamp = 7200;
    let predecessor = view.policy.policy_hash().unwrap();
    let mut policy = view.policy.clone();
    policy.predecessor_policy_hash = predecessor;
    policy.policy_version += 1;
    policy.activation_height = 100;
    view.active_upgrade = Some(EnclaveUpgradeV1 {
        proposal_id: U256::from(1),
        activation_height: 100,
        mrenclave: B256::repeat_byte(88),
        successor_policy_hash: policy.policy_hash().unwrap(),
        predecessor_policy_hash: predecessor,
    });
    view.successor_policy = Some(FinalizedStagedSuccessorPolicyV1 {
        finalized_height: 100,
        finalized_hash: view.checkpoint.hash,
        proposal_id: U256::from(1),
        policy,
    });
    view
}

fn lease() -> SuccessorLeaseV1 {
    SuccessorLeaseV1 {
        binding_id: B256::repeat_byte(44),
        valid_until: 14400,
    }
}

#[tokio::test]
async fn cold_strict_prepare_uses_finalized_old_binding_and_real_candidate_signatures() {
    use alloy_consensus::{transaction::RlpEcdsaDecodableTx as _, TxEip1559};
    use alloy_sol_types::SolCall as _;
    use outbe_primitives::tee_attestation_v1::*;
    use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
    let root = tempfile::tempdir().unwrap();
    let mut view = cold_view();
    // The finalized active policy can already be the successor.
    view.policy = view.successor_policy.as_ref().unwrap().policy.clone();
    let chain = Chain::new(view, root.path());
    let candidate = support::context(9);
    let signer = Signer::default();
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let (attempt, step) =
        prepare_cold_successor_v1(&chain, &signer, &candidate, &mut journal, lease())
            .await
            .unwrap();
    let record = journal.load().unwrap().unwrap();
    assert_eq!(
        step,
        SuccessorStep::Submitted {
            transaction_hash: record.tx_hash,
            replayed: false
        }
    );
    assert_eq!(record.kind, ServiceTxKindV1::UpgradePrepare);
    assert_eq!(record.nonce, 4);
    assert_eq!(
        record.public_intent.target,
        candidate::source_target(chain.view.binding.as_ref().unwrap())
    );
    assert_eq!(attempt.source, chain.view.binding.clone().unwrap());
    assert_eq!(
        attempt.prepare_intent.operation,
        AttestationOperationV1::PrepareEnclaveUpgrade
    );
    assert_eq!(attempt.prepare_intent.binding_version, 2);
    assert_eq!(attempt.prepare_intent.transition_nonce, 1);
    let mut raw = &record.raw_signed_tx[1..];
    let tx = TxEip1559::rlp_decode_signed(&mut raw).unwrap();
    let call = ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&tx.tx().input).unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(&call.evidence).unwrap();
    assert_eq!(evidence.intent(), &attempt.prepare_intent);
    assert!(attempt
        .prepare_intent
        .verify_node_signature(call.nodeSignature.as_ref().try_into().unwrap()));
    assert!(attempt
        .prepare_intent
        .verify_enclave_signature(call.enclaveSignature.as_ref().try_into().unwrap()));
    assert!(!candidate.offer_key_ready().unwrap());
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cold_prepare_rejects_non_strict_early_unauthorized_and_stale_candidates_without_writes() {
    for case in 0..12 {
        let root = tempfile::tempdir().unwrap();
        let mut view = cold_view();
        let mut signer = Signer::default();
        let candidate = support::context(if case == 8 { 8 } else { 9 });
        let mut requested = lease();
        match case {
            0 => view.active_upgrade = None,
            1 => {
                view.checkpoint.height = 99;
                view.schedule.finalized_height = 99;
                view.successor_policy.as_mut().unwrap().finalized_height = 99;
            }
            2 => view.active_upgrade.as_mut().unwrap().activation_height = 101,
            3 => view.active_upgrade.as_mut().unwrap().successor_policy_hash = B256::ZERO,
            4 => view.binding.as_mut().unwrap().node_host_authorization_hash = B256::ZERO,
            5 => view.binding.as_mut().unwrap().node_id_hash = B256::ZERO,
            6 => signer = Signer(k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap()),
            7 => view.binding = None,
            8 => {}
            9 => requested.binding_id = view.binding.as_ref().unwrap().binding_id,
            10 => view.binding.as_mut().unwrap().policy_hash = B256::repeat_byte(99),
            11 => view.successor_policy.as_mut().unwrap().finalized_hash = B256::ZERO,
            _ => unreachable!(),
        }
        let chain = Chain::new(view, root.path());
        let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
        assert!(
            prepare_cold_successor_v1(&chain, &signer, &candidate, &mut journal, requested)
                .await
                .is_err(),
            "case {case}"
        );
        assert!(journal.load().unwrap().is_none(), "case {case}");
        assert!(chain.sent.lock().unwrap().is_empty(), "case {case}");
    }
}

#[tokio::test]
async fn cold_pending_prepare_replays_or_replaces_only_after_finalized_nonce_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let signer = Signer::default();
    let candidate = support::context(9);
    let replacement = support::context(10);
    let mut chain = Chain::new(cold_view(), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    prepare_cold_successor_v1(&chain, &signer, &candidate, &mut journal, lease())
        .await
        .unwrap();
    let previous = journal.load().unwrap().unwrap();
    let before = std::fs::read(&chain.journal_path).unwrap();
    assert!(matches!(
        prepare_cold_successor_v1(&chain, &signer, &candidate, &mut journal, lease()).await,
        Err(SuccessorError::PendingOperation)
    ));
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    assert_eq!(
        reconcile_successor_v1(&chain, &signer, &candidate, &mut journal)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            transaction_hash: previous.tx_hash,
            replayed: true
        }
    );
    {
        let sent = chain.sent.lock().unwrap();
        assert_eq!(sent[0], sent[1]);
    }
    chain.nonces = [4, 5, 5];
    assert!(matches!(
        prepare_cold_successor_v1(&chain, &signer, &replacement, &mut journal, lease()).await,
        Err(SuccessorError::PendingOperation)
    ));
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    chain.nonces = [4; 3];
    let mut next_lease = lease();
    next_lease.binding_id = B256::repeat_byte(45);
    prepare_cold_successor_v1(&chain, &signer, &replacement, &mut journal, next_lease)
        .await
        .unwrap();
    let next = journal.load().unwrap().unwrap();
    assert_eq!(next.nonce, previous.nonce);
    assert_ne!(next.tx_hash, previous.tx_hash);
    assert_eq!(next.public_intent.target, previous.public_intent.target);
}

#[tokio::test]
async fn cold_recovery_reconciles_finalized_old_onboarding_without_old_key_import() {
    let root = tempfile::tempdir().unwrap();
    let signer = Signer::default();
    let candidate = support::context(9);
    let mut chain = Chain::new(cold_view(), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    {
        let old = support::context(8);
        support::finalized_onboarding(
            &mut chain,
            &old,
            &signer,
            &mut journal,
            ServiceTxKindV1::Register,
        );
    }
    prepare_cold_successor_v1(&chain, &signer, &candidate, &mut journal, lease())
        .await
        .unwrap();
    assert_eq!(
        journal.load().unwrap().unwrap().kind,
        ServiceTxKindV1::UpgradePrepare
    );
    assert!(!candidate.offer_key_ready().unwrap());
}

#[tokio::test]
async fn cold_prepare_does_not_overwrite_an_unresolved_old_registration() {
    let root = tempfile::tempdir().unwrap();
    let signer = Signer::default();
    let candidate = support::context(9);
    let mut chain = Chain::new(cold_view(), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    {
        let old = support::context(8);
        support::finalized_onboarding(
            &mut chain,
            &old,
            &signer,
            &mut journal,
            ServiceTxKindV1::Register,
        );
    }
    let mut pending = journal.load().unwrap().unwrap();
    pending.state = ServiceTxStateV1::Pending;
    pending.reconciled_checkpoint = None;
    journal.begin(pending).unwrap();
    // Finalized Registry does not confirm this signed registration.
    chain.binding().binding_id = B256::repeat_byte(55);
    chain.nonces = [3; 3];
    let before = std::fs::read(&chain.journal_path).unwrap();
    assert!(matches!(
        prepare_cold_successor_v1(&chain, &signer, &candidate, &mut journal, lease()).await,
        Err(SuccessorError::PendingOperation)
    ));
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    assert!(chain.sent.lock().unwrap().is_empty());
}
