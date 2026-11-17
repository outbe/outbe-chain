use super::*;
use crate::lifecycle::renewal::tests::support::{self, Chain, Signer};
use crate::lifecycle::*;
use crate::registry_view::FinalizedStagedSuccessorPolicyV1;
use alloy_primitives::B256;
#[path = "../../../tests/in_process_dkg_and_admission/finality.rs"]
mod finality;
mod fixtures;

#[tokio::test]
async fn prepare_candidate_key_import_verifies_real_genesis_bls_mpt_before_key_ready_and_transition(
) {
    use crate::{resident::ResidentOfferMaterial, upgrade_transfer::UpgradeKeyProofV1};
    outbe_simplex::config::init_consensus_chain_id(676).unwrap();
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = fixtures::AdmissionFixture::descriptor(&genesis);
    let source = fixtures::context(8, &descriptor);
    let candidate = fixtures::context(9, &descriptor);
    let root = tempfile::tempdir().unwrap();
    source
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                vec![42; 96],
                B256::from(source.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let public = source.network_encryption_public().unwrap();
    let mut view = support::view(&source);
    view.network_encryption_public_key = public.into();
    let mut policy = view.policy.clone();
    policy.predecessor_policy_hash = policy.policy_hash().unwrap();
    policy.policy_version += 1;
    policy.activation_height = 100;
    view.successor_policy = Some(FinalizedStagedSuccessorPolicyV1 {
        finalized_height: view.checkpoint.height,
        finalized_hash: view.checkpoint.hash,
        proposal_id: alloy_primitives::U256::from(1),
        policy,
    });
    let mut chain = Chain::new(view, root.path());
    let signer = Signer::default();
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let (attempt, outcome) = prepare_successor_v1(
        &chain,
        &signer,
        &candidate,
        &mut journal,
        SuccessorLeaseV1 {
            binding_id: B256::repeat_byte(44),
            valid_until: 10000,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        SuccessorStep::Submitted {
            replayed: false,
            ..
        }
    ));
    chain.view.candidate = journal.load().unwrap().unwrap().public_intent.candidate;
    let material = source
        .prepare_no_attestation_dev_onboarding_artifact_v2(attempt.context)
        .unwrap();
    let admission = fixtures::AdmissionFixture::new(&genesis, &attempt.context);
    let supplied = LifecycleOnboardingProofV1 {
        artifact: material.onboarding_artifact.into(),
        proof: UpgradeKeyProofV1 {
            anchor_outcome: admission.anchor.clone().into(),
            committee_transitions: admission
                .transitions
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            admission: admission.proof.encode_canonical().unwrap().into(),
        },
    };
    let mut bad = supplied.clone();
    let mut forged = admission.proof.clone();
    forged.registry_storage[0].nodes[0][0] ^= 1;
    bad.proof.admission = forged.encode_canonical().unwrap().into();
    chain.upgrade_reply = Some(bad);
    assert!(
        provision_successor_v1(&chain, &signer, &candidate, &attempt, &supplied.proof)
            .await
            .is_err()
    );
    assert!(!candidate.offer_key_ready().unwrap());
    chain.upgrade_reply = Some(supplied.clone());
    assert_eq!(
        provision_successor_v1(&chain, &signer, &candidate, &attempt, &supplied.proof)
            .await
            .unwrap(),
        SuccessorStep::KeyReady
    );
    assert_eq!(candidate.network_encryption_public().unwrap(), public);
    let intent = candidate::transition_intent::<Chain, Signer>(
        &candidate,
        chain.view.binding.as_ref().unwrap(),
        &chain.view.successor_policy.as_ref().unwrap().policy,
        attempt.prepare_intent.binding_id,
        10000,
    )
    .unwrap();
    let proof = candidate.transition_key_ready_proof_v1(&intent).unwrap();
    assert_eq!(proof.transition_intent_hash, intent.intent_hash().unwrap());
    assert_eq!(proof.resident_offer_public, public);
    proof.verify_for_transition(&intent, public).unwrap();
    assert!(matches!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            replayed: false,
            ..
        }
    ));
    let saved = journal.load().unwrap().unwrap();
    assert_eq!(saved.kind, ServiceTxKindV1::UpgradeTransition);
    assert_eq!(
        reconcile_successor_v1(&chain, &signer, &candidate, &mut journal)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            transaction_hash: saved.tx_hash,
            replayed: true
        }
    );
}

fn successor_view(source: &crate::in_process::InProcessTeeContext) -> LifecycleRegistryStateV1 {
    let mut view = support::view(source);
    let mut policy = view.policy.clone();
    policy.predecessor_policy_hash = policy.policy_hash().unwrap();
    policy.policy_version += 1;
    policy.activation_height = 100;
    view.successor_policy = Some(FinalizedStagedSuccessorPolicyV1 {
        finalized_height: view.checkpoint.height,
        finalized_hash: view.checkpoint.hash,
        proposal_id: alloy_primitives::U256::from(1),
        policy,
    });
    view
}

#[tokio::test]
async fn unbroadcast_lost_candidate_replaces_same_nonce_without_changing_source() {
    let root = tempfile::tempdir().unwrap();
    let source = support::context(8);
    let old = support::context(9);
    let live = support::context(10);
    let signer = Signer::default();
    let mut chain = Chain::new(successor_view(&source), root.path());
    chain.upgrade_nonce = 7;
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    prepare_successor_v1(
        &chain,
        &signer,
        &old,
        &mut journal,
        SuccessorLeaseV1 {
            binding_id: B256::repeat_byte(44),
            valid_until: 10000,
        },
    )
    .await
    .unwrap();
    let previous = journal.load().unwrap().unwrap();
    assert_eq!(previous.public_intent.candidate.unwrap().nonce, 8);
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::ReplaceLostIdentity { nonce: 4 }
    );
    let source_target = previous.public_intent.target.clone();
    prepare_successor_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        SuccessorLeaseV1 {
            binding_id: B256::repeat_byte(45),
            valid_until: 10000,
        },
    )
    .await
    .unwrap();
    let replaced = journal.load().unwrap().unwrap();
    assert_eq!(replaced.nonce, previous.nonce);
    assert_ne!(replaced.tx_hash, previous.tx_hash);
    assert_eq!(replaced.public_intent.target, source_target);
    chain.nonces = [4, 5, 5];
    let before = std::fs::read(&chain.journal_path).unwrap();
    assert_eq!(
        reconcile_successor_v1(&chain, &signer, &live, &mut journal)
            .await
            .unwrap(),
        SuccessorStep::AwaitingFinality
    );
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    chain.nonces = [5, 5, 5];
    assert_eq!(
        reconcile_successor_v1(&chain, &signer, &live, &mut journal)
            .await
            .unwrap(),
        SuccessorStep::RebuildRequired { nonce: 5 }
    );
}

#[tokio::test]
async fn lost_or_rebound_candidate_is_cancelled_with_exact_context_and_durable_intent() {
    let root = tempfile::tempdir().unwrap();
    let source = support::context(8);
    let candidate = support::context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(successor_view(&source), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let (attempt, _) = prepare_successor_v1(
        &chain,
        &signer,
        &candidate,
        &mut journal,
        SuccessorLeaseV1 {
            binding_id: B256::repeat_byte(44),
            valid_until: 10000,
        },
    )
    .await
    .unwrap();
    chain.view.candidate = journal.load().unwrap().unwrap().public_intent.candidate;
    let expected = chain.view.candidate.unwrap().context_hash;
    assert_eq!(
        reconcile_candidate_v1(&chain.view, None, None),
        SuccessorStep::CancelRequired {
            context_hash: expected
        }
    );
    assert_eq!(
        reconcile_candidate_v1(&chain.view, Some(&attempt), Some(&candidate)),
        SuccessorStep::Prepared
    );
    let saved = journal.load().unwrap().unwrap();
    chain.receipt = Some(LifecycleReceiptV1 {
        tx_hash: saved.tx_hash,
        block_height: chain.view.checkpoint.height,
        block_hash: chain.view.checkpoint.hash,
        success: true,
    });
    chain.binding().binding_id = B256::repeat_byte(55);
    assert_eq!(
        reconcile_candidate_v1(&chain.view, Some(&attempt), Some(&candidate)),
        SuccessorStep::CancelRequired {
            context_hash: expected
        }
    );
    let before = std::fs::read(&chain.journal_path).unwrap();
    let receipt = chain.receipt.take();
    assert!(
        cancel_successor_v1(&chain, &signer, &source, &mut journal, expected)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    chain.receipt = receipt;
    assert_eq!(
        reconcile_successor_v1(&chain, &signer, &candidate, &mut journal)
            .await
            .unwrap(),
        SuccessorStep::CancelRequired {
            context_hash: expected
        }
    );
    assert!(matches!(
        cancel_successor_v1(&chain, &signer, &source, &mut journal, expected)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            replayed: false,
            ..
        }
    ));
    assert_eq!(
        journal.load().unwrap().unwrap().kind,
        ServiceTxKindV1::UpgradeCancel
    );
    chain.view.candidate = None;
    chain.receipt = None;
    assert_eq!(
        reconcile_successor_v1(&chain, &signer, &source, &mut journal)
            .await
            .unwrap(),
        SuccessorStep::Finalized
    );
}

#[tokio::test]
async fn strict_upgrade_allows_late_recovery_but_expired_pre_activation_source_needs_paid_rejoin() {
    let root = tempfile::tempdir().unwrap();
    let source = support::context(8);
    let candidate = support::context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(successor_view(&source), root.path());
    chain.timestamp(7200);
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let lease = SuccessorLeaseV1 {
        binding_id: B256::repeat_byte(44),
        valid_until: 14400,
    };
    assert!(matches!(
        prepare_successor_v1(&chain, &signer, &candidate, &mut journal, lease).await,
        Err(SuccessorError::SourceLeaseExpiredNeedsRejoin)
    ));
    assert!(journal.load().unwrap().is_none());
    chain.view.checkpoint.height = 100;
    chain.view.schedule.finalized_height = 100;
    chain
        .view
        .successor_policy
        .as_mut()
        .unwrap()
        .finalized_height = 100;
    let hash = chain
        .view
        .successor_policy
        .as_ref()
        .unwrap()
        .policy
        .policy_hash()
        .unwrap();
    chain.view.active_upgrade = Some(outbe_primitives::enclave_upgrade::EnclaveUpgradeV1 {
        proposal_id: alloy_primitives::U256::from(1),
        activation_height: 100,
        mrenclave: B256::repeat_byte(88),
        successor_policy_hash: hash,
        predecessor_policy_hash: chain.view.policy.policy_hash().unwrap(),
    });
    let (attempt, _) = prepare_successor_v1(&chain, &signer, &candidate, &mut journal, lease)
        .await
        .unwrap();
    chain.view.candidate = journal.load().unwrap().unwrap().public_intent.candidate;
    candidate
        .activate_for_test(
            crate::resident::ResidentOfferMaterial::for_test(
                vec![42; 96],
                B256::from(candidate.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    chain.view.network_encryption_public_key =
        candidate.network_encryption_public().unwrap().into();
    assert!(matches!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            replayed: false,
            ..
        }
    ));
}

#[test]
fn transition_collateral_clamp_preserves_the_minimum_lease_boundary() {
    let live = support::context(8);
    let policy = support::view(&live).policy;
    for (expiration, expected) in [
        (13000, Some(9400)),
        (10800, Some(7200)),
        (10799, None),
        (0, None),
    ] {
        assert_eq!(
            transition::clamped_deadline::<Chain, Signer>(3600, &policy, expiration).ok(),
            expected
        );
    }
    assert!(transition::clamped_deadline::<Chain, Signer>(u64::MAX, &policy, u64::MAX).is_err());
}
