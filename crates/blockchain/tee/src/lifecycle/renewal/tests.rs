use super::*;

// Lease expiry must route to paid rejoin, never another renew transaction.
#[test]
fn schedule_preserves_half_lease_window_and_exclusive_expiry() {
    for (now, expected) in [
        (
            99,
            RenewalStep::NotDue {
                opens_at_timestamp: 100,
            },
        ),
        (100, RenewalStep::Due),
        (199, RenewalStep::Due),
        (200, RenewalStep::LeaseAlreadyExpiredNeedsRejoin),
        (201, RenewalStep::LeaseAlreadyExpiredNeedsRejoin),
    ] {
        assert_eq!(renewal_schedule_v1(now, 200, 200).unwrap(), expected);
    }
    for lease in [0, 3] {
        assert!(renewal_schedule_v1(10, 200, lease).is_err());
    }
}

pub(crate) mod support;
use crate::lifecycle::*;
use alloy_consensus::{transaction::RlpEcdsaDecodableTx as _, TxEip1559};
use alloy_sol_types::SolCall as _;
use outbe_primitives::{tee_attestation_v1::*, tee_registry_abi_v1::ITeeRegistryV1};
use support::*;

#[tokio::test]
async fn zero_balance_renewal_is_durable_zero_fee_exact_replay_and_nonce_rebuild() {
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let first = renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    assert!(matches!(
        first,
        RenewalStep::Submitted {
            replayed: false,
            ..
        }
    ));
    let saved = journal.load().unwrap().unwrap();
    let signed = TxEip1559::rlp_decode_signed(&mut &saved.raw_signed_tx[1..]).unwrap();
    let tx = signed.tx();
    assert_eq!(
        (tx.max_priority_fee_per_gas, tx.value, tx.max_fee_per_gas),
        (0, alloy_primitives::U256::ZERO, 17)
    );
    assert_eq!(tx.gas_limit, renewal_gas_limit_v1().unwrap());
    let call = ITeeRegistryV1::renewEnclaveCall::abi_decode(&tx.input).unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(&call.evidence).unwrap();
    assert_eq!(
        (
            evidence.intent().registration_version,
            evidence.intent().renewal_nonce,
            evidence.intent().requested_valid_until
        ),
        (2, 1, 14400)
    );
    assert!(matches!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::Submitted { replayed: true, .. }
    ));
    {
        let sent = chain.sent.lock().unwrap();
        assert_eq!(sent[0], sent[1]);
    }
    chain.nonces = [4, 5, 5];
    let before = std::fs::read(&chain.journal_path).unwrap();
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::AwaitingFinality
    );
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    chain.nonces = [5, 6, 7];
    assert!(matches!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::Submitted {
            replayed: false,
            ..
        }
    ));
    assert_eq!(journal.load().unwrap().unwrap().nonce, 7);
    chain.timestamp(7200);
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::LeaseAlreadyExpiredNeedsRejoin
    );
    assert_eq!(chain.sent.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn finalized_renewal_is_not_duplicated_and_identity_policy_guards_precede_signing() {
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    let record = journal.load().unwrap().unwrap();
    chain.binding().registration_version = 2;
    chain.binding().renewal_nonce = 1;
    chain.binding().valid_until = 14400;
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::NotDue {
            opens_at_timestamp: 10800
        }
    );
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
    assert_eq!(
        journal.load().unwrap().unwrap().state,
        ServiceTxStateV1::Completed
    );
    chain.binding().recipient_x25519 = alloy_primitives::B256::repeat_byte(99);
    assert!(matches!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await,
        Err(RenewalError::BindingMismatch)
    ));
    assert_eq!(record.nonce, 4);
}

#[tokio::test]
async fn upgrade_phases_preserve_prepared_source_renewal_and_exact_promoted_exception() {
    use alloy_primitives::B256;
    for phase in [
        RenewalUpgradePhaseV1::Idle,
        RenewalUpgradePhaseV1::CandidatePrepared,
        RenewalUpgradePhaseV1::KeyProvisioned,
        RenewalUpgradePhaseV1::CandidateKeyReady,
        RenewalUpgradePhaseV1::SubmissionPrepared,
        RenewalUpgradePhaseV1::Submitted,
        RenewalUpgradePhaseV1::Finalized,
        RenewalUpgradePhaseV1::TerminalMissedCutoff,
    ] {
        let root = tempfile::tempdir().unwrap();
        let live = context(9);
        let signer = Signer::default();
        let mut chain = Chain::new(view(&live), root.path());
        if phase == RenewalUpgradePhaseV1::Idle {
            chain.view.checkpoint.base_fee_per_gas = alloy_primitives::U256::ZERO;
        }
        chain.view.candidate = Some(LifecycleUpgradeCandidateV1 {
            context_hash: B256::repeat_byte(8),
            valid_until: 10000,
            source_binding_id: chain.view.binding.as_ref().unwrap().binding_id,
            target_hash: B256::repeat_byte(9),
            nonce: 1,
        });
        let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
        let outcome = renew_once_v1(&chain, &signer, &live, &mut journal, phase)
            .await
            .unwrap();
        if matches!(
            phase,
            RenewalUpgradePhaseV1::Idle | RenewalUpgradePhaseV1::CandidatePrepared
        ) {
            assert!(matches!(outcome, RenewalStep::Submitted { .. }));
            if phase == RenewalUpgradePhaseV1::Idle {
                let record = journal.load().unwrap().unwrap();
                let signed = TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
                assert_eq!(signed.tx().max_fee_per_gas, 7);
            }
        } else {
            assert_eq!(outcome, RenewalStep::BlockedByActiveUpgrade);
            assert!(journal.load().unwrap().is_none());
            assert!(chain.sent.lock().unwrap().is_empty());
        }
    }
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let policy = chain.view.binding.as_ref().unwrap().policy_hash;
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Promoted {
                candidate_manifest_hash: B256::ZERO,
                successor_policy_hash: policy
            }
        )
        .await
        .unwrap(),
        RenewalStep::BlockedByActiveUpgrade
    );
    let phase = RenewalUpgradePhaseV1::Promoted {
        candidate_manifest_hash: live.manifest().authorization_hash().unwrap(),
        successor_policy_hash: policy,
    };
    assert!(matches!(
        renew_once_v1(&chain, &signer, &live, &mut journal, phase)
            .await
            .unwrap(),
        RenewalStep::Submitted { .. }
    ));
}

#[tokio::test]
async fn expired_pending_renew_waits_without_replay_then_releases_same_identity_rejoin_slot() {
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    let original = journal.load().unwrap().unwrap();
    chain.timestamp(7200);
    chain.nonces = [4, 5, 5];
    let before = std::fs::read(&chain.journal_path).unwrap();
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::AwaitingFinality
    );
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
    chain.nonces = [4, 4, 4];
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::LeaseAlreadyExpiredNeedsRejoin
    );
    assert_eq!(
        journal.load().unwrap().unwrap().state,
        ServiceTxStateV1::RebuildRequired
    );
    use super::prepare::{evidence_v1, signed_record, target_v1, ServiceCallV1};
    use alloy_primitives::B256;
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign {
            nonce: original.nonce
        }
    );
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign {
            nonce: original.nonce
        }
    );
    let mut raw = &original.raw_signed_tx[1..];
    let signed = TxEip1559::rlp_decode_signed(&mut raw).unwrap();
    let previous = ITeeRegistryV1::renewEnclaveCall::abi_decode(&signed.tx().input).unwrap();
    let mut intent = AttestationEvidenceV1::decode_canonical(&previous.evidence)
        .unwrap()
        .intent()
        .clone();
    intent.operation = AttestationOperationV1::RegisterEnclave;
    intent.binding_version += 1;
    intent.binding_id = B256::repeat_byte(55);
    intent.requested_valid_until = 14400;
    let (evidence, signature) =
        evidence_v1::<std::io::Error, k256::ecdsa::Error>(&live, &intent, &chain.view.policy, 7200)
            .unwrap();
    let node = signer
        .sign_node_hash(intent.intent_hash().unwrap())
        .unwrap();
    let calldata = ITeeRegistryV1::registerEnclaveCall {
        validatorNodeBinding: Default::default(),
        validatorSignature: Default::default(),
        nodeBindingSignature: Default::default(),
        evidence: evidence.into(),
        nodeSignature: node.to_vec().into(),
        enclaveSignature: signature.to_vec().into(),
    }
    .abi_encode();
    let rejoin = signed_record::<Chain, Signer>(
        &signer,
        chain.view.checkpoint,
        original.nonce,
        ServiceCallV1 {
            gas_limit: 1_000_000,
            calldata,
            kind: ServiceTxKindV1::Register,
            intent: ServiceTxIntentV1 {
                target: target_v1(&intent).unwrap(),
                candidate: None,
            },
        },
    )
    .unwrap();
    journal.begin(rejoin.clone()).unwrap();
    assert_eq!(journal.load().unwrap(), Some(rejoin));
}
