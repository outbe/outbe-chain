use super::*;
use crate::lifecycle::renewal::tests::support::{self, Chain, Signer};
use crate::lifecycle::renewal::RenewalError;
use crate::lifecycle::test_finality as finality;
use crate::lifecycle::*;
use crate::registry_view::FinalizedStagedSuccessorPolicyV1;
use alloy_primitives::{B256, U256};
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
    let (mut attempt, outcome) = prepare_successor_v1(
        &chain,
        &signer,
        (&source, &candidate),
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
    let admission = fixtures::AdmissionFixture::new(&genesis, &attempt.context, 42);
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
    let mut forged_witness = admission.proof.clone();
    forged_witness.registry_storage[0].nodes[0][0] ^= 1;
    let mut forged = supplied.proof.clone();
    forged.admission = forged_witness.encode_canonical().unwrap().into();
    chain.view.checkpoint.base_fee_per_gas = U256::from(99);
    chain.upgrade_reply = Some(supplied.clone());
    *chain.upgrade_checkpoint.lock().unwrap() = None;
    assert!(
        provision_successor_v1(&chain, &signer, &candidate, &attempt, &forged)
            .await
            .is_err()
    );
    assert!(!candidate.offer_key_ready().unwrap());
    let priced_fee = chain.upgrade_checkpoint.lock().unwrap().unwrap();
    assert_eq!(priced_fee.base_fee_per_gas, U256::from(42));
    assert_ne!(
        priced_fee.base_fee_per_gas,
        chain.view.checkpoint.base_fee_per_gas
    );
    let proof_header = admission_header(&supplied.proof);
    assert_eq!(proof_header.base_fee_per_gas, Some(42));
    let original = chain.view.checkpoint;
    pin_finality(&mut chain, proof_header.number - 1, B256::repeat_byte(0x44));
    *chain.upgrade_checkpoint.lock().unwrap() = None;
    assert!(matches!(
        provision_successor_v1(&chain, &signer, &candidate, &attempt, &supplied.proof).await,
        Err(SuccessorError::Lifecycle(RenewalError::BindingMismatch))
    ));
    assert!(chain.upgrade_checkpoint.lock().unwrap().is_none());
    assert!(!candidate.offer_key_ready().unwrap());
    let mut tampered = supplied.proof.clone();
    let mut admission = tampered.admission.to_vec();
    admission[1] ^= 0xff;
    tampered.admission = admission.into();
    pin_finality(&mut chain, proof_header.number + 8, B256::repeat_byte(0x46));
    chain.view.checkpoint.state_root = B256::repeat_byte(0x47);
    assert!(
        provision_successor_v1(&chain, &signer, &candidate, &attempt, &tampered)
            .await
            .is_err()
    );
    assert!(!candidate.offer_key_ready().unwrap());
    assert_eq!(
        provision_successor_v1(&chain, &signer, &candidate, &attempt, &supplied.proof)
            .await
            .unwrap(),
        SuccessorStep::KeyReady
    );
    let seen = chain.upgrade_checkpoint.lock().unwrap().unwrap();
    assert_eq!(
        (
            seen.height,
            seen.hash,
            seen.state_root,
            seen.base_fee_per_gas
        ),
        (
            proof_header.number,
            proof_header.hash_slow(),
            proof_header.state_root,
            U256::from(42)
        )
    );
    assert!(seen.height < chain.view.checkpoint.height);
    assert_ne!(
        seen.base_fee_per_gas,
        chain.view.checkpoint.base_fee_per_gas
    );
    assert_ne!(
        (seen.hash, seen.state_root),
        (chain.view.checkpoint.hash, chain.view.checkpoint.state_root)
    );
    chain.view.candidate.as_mut().unwrap().context_hash = B256::repeat_byte(0x99);
    assert!(matches!(
        provision_successor_v1(&chain, &signer, &candidate, &attempt, &supplied.proof).await,
        Err(SuccessorError::CandidateChanged)
    ));
    chain.view.candidate = journal.load().unwrap().unwrap().public_intent.candidate;
    chain.view.checkpoint = original;
    chain.view.schedule.finalized_height = original.height;
    chain.view.schedule.finalized_hash = original.hash;
    chain.view.schedule.finalized_timestamp = original.timestamp;
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
    let signed = decode_intent(&saved).unwrap();
    let source = candidate::source_target(chain.view.binding.as_ref().unwrap());
    assert_ne!(saved.public_intent.target, source);
    assert_ne!(
        source.enclave_id,
        candidate.manifest().enclave_id().unwrap()
    );
    assert_eq!(
        saved.public_intent.target,
        crate::lifecycle::renewal::prepare::target_v1(&signed).unwrap()
    );
    assert_eq!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            transaction_hash: saved.tx_hash,
            replayed: true
        }
    );
    assert_eq!(
        reconcile_successor_v1(&chain, &signer, &candidate, &mut journal)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            transaction_hash: saved.tx_hash,
            replayed: true
        }
    );
    let path = chain.journal_path.clone();
    let mut wire: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let unclamped = wire["record"]["public_intent"]["target"]["valid_until"].clone();
    wire["record"]["public_intent"]["target"]["valid_until"] =
        serde_json::json!(unclamped.as_u64().unwrap() - 500);
    std::fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
    assert!(matches!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt).await,
        Err(SuccessorError::CandidateMismatch)
    ));
    assert_eq!(journal.load().unwrap().unwrap().tx_hash, saved.tx_hash);
    wire["record"]["public_intent"]["target"]["valid_until"] = unclamped;
    std::fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
    let full_until = attempt.prepare_intent.requested_valid_until;
    std::fs::remove_file(&path).unwrap();
    attempt.prepare_intent.requested_valid_until = full_until - 500;
    chain.view.candidate.as_mut().unwrap().valid_until = full_until - 500;
    assert!(matches!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            replayed: false,
            ..
        }
    ));
    let shortened = journal.load().unwrap().unwrap();
    let shortened_intent = decode_intent(&shortened).unwrap();
    assert_eq!(shortened.public_intent.target.valid_until, full_until - 500);
    assert_eq!(
        shortened.public_intent.target,
        crate::lifecycle::renewal::prepare::target_v1(&shortened_intent).unwrap()
    );
    attempt.prepare_intent.requested_valid_until = full_until;
    chain.view.candidate.as_mut().unwrap().valid_until = full_until;
    assert_eq!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            transaction_hash: shortened.tx_hash,
            replayed: true
        }
    );
    let saved = shortened;
    let mut wire: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut foreign = saved.public_intent.target.clone();
    foreign.enclave_id = B256::repeat_byte(0x5a);
    foreign.binding_id = B256::repeat_byte(0x5b);
    wire["record"]["public_intent"]["target"] = serde_json::to_value(&foreign).unwrap();
    std::fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
    assert!(matches!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt).await,
        Err(SuccessorError::CandidateMismatch)
    ));
    assert_eq!(journal.load().unwrap().unwrap().tx_hash, saved.tx_hash);
    wire["record"]["public_intent"]["target"] =
        serde_json::to_value(&saved.public_intent.target).unwrap();
    std::fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
    let displaced_nonce = saved.nonce + 1;
    chain.nonces = [displaced_nonce; 3];
    assert_eq!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::RebuildRequired {
            nonce: displaced_nonce
        }
    );
    let rebuilt = journal.load().unwrap().unwrap();
    assert_eq!(rebuilt.state, ServiceTxStateV1::RebuildRequired);
    assert_eq!(rebuilt.tx_hash, saved.tx_hash);
    assert_eq!(rebuilt.nonce, saved.nonce);
    let sent = chain.sent.lock().unwrap().len();
    assert!(matches!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::Submitted {
            replayed: false,
            ..
        }
    ));
    let resigned = journal.load().unwrap().unwrap();
    assert_eq!(resigned.kind, ServiceTxKindV1::UpgradeTransition);
    assert_eq!(resigned.state, ServiceTxStateV1::Pending);
    assert_eq!(resigned.nonce, displaced_nonce);
    assert_ne!(resigned.tx_hash, saved.tx_hash);
    assert_eq!(chain.sent.lock().unwrap().len(), sent + 1);
    let staged = chain.view.successor_policy.as_mut().unwrap();
    staged.policy.policy_version += 1;
    staged.policy.activation_height = 1;
    assert_ne!(
        staged.policy.policy_hash().unwrap(),
        attempt.prepare_intent.policy_hash
    );
    apply_target(chain.binding(), &resigned.public_intent.target);
    chain.view.candidate = None;
    assert_eq!(
        transition_successor_v1(&chain, &signer, &candidate, &mut journal, &attempt)
            .await
            .unwrap(),
        SuccessorStep::Finalized
    );
    let finalized = journal.load().unwrap().unwrap();
    assert_eq!(finalized.tx_hash, resigned.tx_hash);
    assert_eq!(finalized.state, ServiceTxStateV1::Completed);
    assert_eq!(chain.sent.lock().unwrap().len(), sent + 1);
}

fn apply_target(
    binding: &mut crate::registry_view::RenewalBindingV1,
    target: &ServiceBindingTargetV1,
) {
    binding.node_id_hash = target.node_id_hash;
    binding.enclave_id = target.enclave_id;
    binding.binding_id = target.binding_id;
    binding.binding_version = target.binding_version;
    binding.registration_version = target.registration_version;
    binding.renewal_nonce = target.renewal_nonce;
    binding.transition_nonce = target.transition_nonce;
    binding.valid_until = target.valid_until;
    binding.policy_hash = target.policy_hash;
    binding.recipient_x25519 = target.recipient_x25519;
}

fn admission_header(proof: &crate::upgrade_transfer::UpgradeKeyProofV1) -> alloy_consensus::Header {
    use alloy_rlp::Decodable as _;
    let witness =
        crate::finalized_admission::FinalizedAdmissionWitnessV1::decode_canonical(&proof.admission)
            .unwrap();
    let mut rlp = witness.admission.header.as_slice();
    let header = alloy_consensus::Header::decode(&mut rlp).unwrap();
    assert!(rlp.is_empty());
    header
}

fn pin_finality(chain: &mut Chain, height: u64, hash: B256) {
    chain.view.checkpoint.height = height;
    chain.view.checkpoint.hash = hash;
    chain.view.schedule.finalized_height = height;
    chain.view.schedule.finalized_hash = hash;
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
        (&source, &old),
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
        (&source, &live),
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
        (&source, &candidate),
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
        prepare_successor_v1(&chain, &signer, (&source, &candidate), &mut journal, lease).await,
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
    let (attempt, _) =
        prepare_successor_v1(&chain, &signer, (&source, &candidate), &mut journal, lease)
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

#[tokio::test]
async fn finalized_onboarding_requires_source_readiness_before_successor_prepare() {
    for (kind, ready) in [
        (ServiceTxKindV1::Register, true),
        (ServiceTxKindV1::Register, false),
        (ServiceTxKindV1::UpgradeTransition, true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let source = support::context(8);
        let candidate = support::context(9);
        let signer = Signer::default();
        if ready {
            support::import_test_key(&source);
        }
        let mut chain = Chain::new(successor_view(&source), root.path());
        let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
        support::finalized_onboarding(&mut chain, &source, &signer, &mut journal, kind);
        let before = std::fs::read(&chain.journal_path).unwrap();
        assert!(matches!(
            prepare_successor_v1(
                &chain,
                &signer,
                (&candidate, &candidate),
                &mut journal,
                SuccessorLeaseV1 {
                    binding_id: B256::repeat_byte(44),
                    valid_until: 10000
                }
            )
            .await,
            Err(SuccessorError::CandidateMismatch)
        ));
        assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
        let result = prepare_successor_v1(
            &chain,
            &signer,
            (&source, &candidate),
            &mut journal,
            SuccessorLeaseV1 {
                binding_id: B256::repeat_byte(44),
                valid_until: 10000,
            },
        )
        .await;
        if ready {
            assert!(
                matches!(
                    result,
                    Ok((
                        _,
                        SuccessorStep::Submitted {
                            replayed: false,
                            ..
                        }
                    ))
                ),
                "{kind:?}: {result:?}"
            );
            assert_eq!(
                journal.load().unwrap().unwrap().kind,
                ServiceTxKindV1::UpgradePrepare
            );
            assert_eq!(chain.sent.lock().unwrap().len(), 1);
            assert!(!candidate.offer_key_ready().unwrap());
        } else {
            assert!(
                matches!(result, Err(SuccessorError::PendingOperation)),
                "{result:?}"
            );
            assert!(chain.sent.lock().unwrap().is_empty());
            assert_eq!(
                journal.load().unwrap().unwrap().state,
                ServiceTxStateV1::Completed
            );
        }
    }
}
