//! Candidate reducer coverage; the runtime activation and lease guard are tested in the supervisor.
use super::*;
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::lifecycle::successor::{
    cancel_successor_v1, prepare_cold_successor_v1, provision_successor_v1, reconcile_candidate_v1,
    reconcile_successor_v1, transition_successor_v1, SuccessorAttemptV1, SuccessorLeaseV1,
    SuccessorStep,
};
use outbe_tee::registry_view::{
    FinalizedStagedSuccessorPolicyV1, NodeBindingSelectorV1, RenewalBindingV1,
};
use outbe_tee::upgrade_transfer::UpgradeKeyProofV1;
use std::collections::HashMap;
use std::sync::Mutex;

struct Overlay {
    nonces: [u64; 3],
    checkpoint: Option<LifecycleCheckpointV1>,
    policy: TeePolicyV1,
    active: outbe_primitives::enclave_upgrade::EnclaveUpgradeV1,
    successor: FinalizedStagedSuccessorPolicyV1,
    binding: Option<RenewalBindingV1>,
    candidate: Option<LifecycleUpgradeCandidateV1>,
    upgrade_nonce: u64,
    proofs: HashMap<B256, LifecycleOnboardingProofV1>,
}
struct Seam<'a> {
    chain: &'a Chain,
    state: &'a Mutex<Overlay>,
}
impl LifecycleChainPort for Seam<'_> {
    type Error = std::io::Error;
    async fn finalized_checkpoint(&self) -> chain::PortResult<LifecycleCheckpointV1> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .checkpoint
            .unwrap_or(self.chain.at))
    }
    async fn registry_state(
        &self,
        at: LifecycleCheckpointV1,
        selector: &NodeBindingSelectorV1,
    ) -> chain::PortResult<LifecycleRegistryStateV1> {
        let mut view = self.chain.registry_state(at, selector).await?;
        let overlay = self.state.lock().unwrap();
        view.policy = overlay.policy.clone();
        view.active_upgrade = Some(overlay.active);
        view.successor_policy = Some(overlay.successor.clone());
        view.candidate = overlay.candidate;
        if let Some(binding) = &overlay.binding {
            view.binding = Some(binding.clone());
        }
        Ok(view)
    }
    async fn pending_upgrade_nonce(
        &self,
        _: LifecycleCheckpointV1,
        _: &NodeBindingSelectorV1,
    ) -> chain::PortResult<u64> {
        Ok(self.state.lock().unwrap().upgrade_nonce)
    }
    async fn validator_record(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> chain::PortResult<Option<outbe_primitives::validator_dto::ValidatorRecord>> {
        self.chain.validator_record(at, account).await
    }
    async fn balance(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> chain::PortResult<U256> {
        self.chain.balance(at, account).await
    }
    async fn account_nonce(&self, _: Address, tag: LifecycleNonceTagV1) -> chain::PortResult<u64> {
        let nonces = self.state.lock().unwrap().nonces;
        Ok(nonces[match tag {
            LifecycleNonceTagV1::Finalized(_) => 0,
            LifecycleNonceTagV1::Latest => 1,
            LifecycleNonceTagV1::Pending => 2,
        }])
    }
    async fn send_raw_transaction(&self, raw: &[u8]) -> chain::PortResult<B256> {
        self.chain.send_raw_transaction(raw).await
    }
    async fn finalized_receipt(
        &self,
        at: LifecycleCheckpointV1,
        hash: B256,
    ) -> chain::PortResult<Option<LifecycleReceiptV1>> {
        self.chain.finalized_receipt(at, hash).await
    }
    async fn onboarding_proof(
        &self,
        at: LifecycleCheckpointV1,
        hash: B256,
    ) -> chain::PortResult<LifecycleOnboardingProofV1> {
        self.chain.onboarding_proof(at, hash).await
    }
    async fn upgrade_key(
        &self,
        _: LifecycleCheckpointV1,
        context: &DcapOnboardingContextV1,
        _: &UpgradeKeyProofV1,
    ) -> chain::PortResult<LifecycleOnboardingProofV1> {
        self.state
            .lock()
            .unwrap()
            .proofs
            .get(&context.context_hash())
            .cloned()
            .ok_or_else(|| {
                LifecyclePortError::Source(std::io::Error::other("upgrade proof missing"))
            })
    }
}

fn fresh_candidate(source: &InProcessTeeContext) -> Arc<InProcessTeeContext> {
    let keys = outbe_tee::resident::ResidentTeeKeys::generate().unwrap();
    let mut manifest = source.manifest().clone();
    manifest.initialization_challenge = B256::random().0;
    manifest.recipient_x25519 = keys.recipient_public();
    manifest.attestation_ed25519 = keys.attestation_pub();
    manifest.noise_responder_x25519 = keys.noise_public();
    Arc::new(
        InProcessTeeContext::new(
            keys,
            *source.identity(),
            manifest,
            proof::descriptor(&finality::FinalityCommitteeFixture::new(10)),
            outbe_tee::in_process::ProofSourceV1::NoHardware,
        )
        .unwrap(),
    )
}

fn overlay_for(chain: &Chain) -> Overlay {
    let predecessor = chain.policy.policy_hash().unwrap();
    let mut policy = chain.policy.clone();
    policy.policy_version += 1;
    policy.predecessor_policy_hash = predecessor;
    policy.activation_height = 1;
    let successor_policy_hash = policy.policy_hash().unwrap();
    Overlay {
        nonces: chain.nonces,
        checkpoint: None,
        active: outbe_primitives::enclave_upgrade::EnclaveUpgradeV1 {
            proposal_id: U256::from(1),
            activation_height: 1,
            mrenclave: B256::repeat_byte(88),
            successor_policy_hash,
            predecessor_policy_hash: predecessor,
        },
        successor: FinalizedStagedSuccessorPolicyV1 {
            finalized_height: chain.at.height,
            finalized_hash: chain.at.hash,
            proposal_id: U256::from(1),
            policy: policy.clone(),
        },
        policy,
        binding: None,
        candidate: None,
        upgrade_nonce: 0,
        proofs: HashMap::new(),
    }
}

fn stage(state: &Mutex<Overlay>, attempt: &SuccessorAttemptV1) {
    let intent = &attempt.prepare_intent;
    let candidate = LifecycleUpgradeCandidateV1 {
        context_hash: attempt.context.context_hash(),
        valid_until: intent.requested_valid_until,
        source_binding_id: attempt.source.binding_id,
        target_hash: intent.upgrade_target_hash().unwrap(),
        nonce: intent.transition_nonce,
    };
    let mut overlay = state.lock().unwrap();
    overlay.upgrade_nonce = candidate.nonce;
    overlay.candidate = Some(candidate);
}

fn seal(
    state: &Mutex<Overlay>,
    donor: &InProcessTeeContext,
    attempt: &SuccessorAttemptV1,
) -> UpgradeKeyProofV1 {
    use outbe_primitives::{
        addresses::TEE_REGISTRY_ADDRESS,
        test_utils::state_proof::{account_mpt_with_proofs, storage_trie},
    };
    use outbe_tee::finalized_admission::upgrade_registry_slots_v1;
    let context = &attempt.context;
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let prepared = donor
        .prepare_no_attestation_dev_onboarding_artifact_v2(*context)
        .unwrap();
    let source = U256::from_be_bytes(attempt.source.binding_id.0);
    let slots = upgrade_registry_slots_v1(context);
    let values = [
        U256::from_be_bytes(context.network_encryption_public),
        U256::from(context.key_epoch),
        U256::from(context.network_encryption_epoch),
        U256::from_be_bytes(context.context_hash().0),
        U256::from(attempt.prepare_intent.requested_valid_until),
        source,
        source,
        U256::from_be_bytes(context.policy_hash.0),
        U256::from(attempt.prepare_intent.transition_nonce),
    ];
    let leaves = slots
        .iter()
        .zip(values)
        .map(|(slot, value)| (U256::from_be_bytes(slot.0), value))
        .collect::<Vec<_>>();
    let (storage_root, openings) = storage_trie(&leaves);
    let account = reth_trie::TrieAccount {
        nonce: 1,
        balance: U256::ZERO,
        storage_root,
        code_hash: B256::repeat_byte(0x91),
    };
    let (state_root, account_proofs) = account_mpt_with_proofs(&[(TEE_REGISTRY_ADDRESS, account)]);
    outbe_consensus::proof::init_consensus_chain_id(crate::TEST_CHAIN_ID).ok();
    let certified = genesis.certify_child_block(finality::CertifiedChildBlockInput {
        epoch: commonware_consensus::types::Epoch::new(0),
        height: 2,
        timestamp: 950,
        state_root,
        extra_data: Vec::new(),
        parent_hash: B256::repeat_byte(0x11),
        base_fee_per_gas: Some(1),
    });
    let witness = proof::registry_witness(
        proof::compact(certified),
        account,
        &account_proofs[&TEE_REGISTRY_ADDRESS],
        slots
            .into_iter()
            .zip(values)
            .zip(openings)
            .map(|((key, value), nodes)| (key, value, nodes)),
    );
    let proof = UpgradeKeyProofV1 {
        anchor_outcome: genesis
            .outcome(commonware_consensus::types::Epoch::new(0))
            .into(),
        committee_transitions: Vec::new(),
        admission: witness.encode_canonical().unwrap().into(),
    };
    state.lock().unwrap().proofs.insert(
        context.context_hash(),
        LifecycleOnboardingProofV1 {
            artifact: prepared.onboarding_artifact.into(),
            proof: proof.clone(),
        },
    );
    proof
}

fn bump_nonce(state: &Mutex<Overlay>, journal: &ServiceTxJournalV1) {
    let nonce = journal.load().unwrap().unwrap().nonce.saturating_add(1);
    state.lock().unwrap().nonces = [nonce; 3];
}

fn selector() -> NodeBindingSelectorV1 {
    NodeBindingSelectorV1::Validator(Signer::address())
}

async fn lease(seam: &Seam<'_>, candidate: &InProcessTeeContext) -> SuccessorLeaseV1 {
    let at = seam.finalized_checkpoint().await.unwrap();
    let view = seam.registry_state(at, &selector()).await.unwrap();
    SuccessorLeaseV1 {
        binding_id: engine::binding_id(candidate, &view).unwrap(),
        valid_until: at.timestamp + view.policy.maximum_lease,
    }
}

fn promoted(source: RenewalBindingV1, intent: &RegistrationIntentV1) -> RenewalBindingV1 {
    RenewalBindingV1 {
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        binding_version: intent.binding_version,
        registration_version: intent.registration_version,
        renewal_nonce: intent.renewal_nonce,
        transition_nonce: intent.transition_nonce,
        valid_until: intent.requested_valid_until,
        policy_hash: intent.policy_hash,
        recipient_x25519: B256::from(intent.recipient_x25519),
        attestation_ed25519: B256::from(intent.attestation_ed25519),
        noise_responder_x25519: B256::from(intent.noise_responder_x25519),
        ..source
    }
}

fn transition_intent(journal: &ServiceTxJournalV1) -> RegistrationIntentV1 {
    use alloy_consensus::{transaction::RlpEcdsaDecodableTx, TxEip1559};
    use alloy_sol_types::SolCall;
    use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
    let record = journal.load().unwrap().unwrap();
    assert_eq!(record.kind, ServiceTxKindV1::UpgradeTransition);
    let tx = TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
    let evidence = ITeeRegistryV1::transitionEnclaveMeasurementCall::abi_decode(&tx.tx().input)
        .unwrap()
        .evidence;
    AttestationEvidenceV1::decode_canonical(&evidence)
        .unwrap()
        .intent()
        .clone()
}

#[tokio::test]
async fn cold_cancel_then_second_candidate_finalizes_in_lifecycle_reducer() {
    let mut harness = Harness::new();
    harness.admit().await;
    let admitted_height = harness.chain.at.height;
    let resident = harness.live.clone();
    let source = harness.chain.binding.clone().unwrap();
    let state = Mutex::new(overlay_for(&harness.chain));
    let (binding, finalized_at, second, second_enclave) = {
        let seam = Seam {
            chain: &harness.chain,
            state: &state,
        };
        let first = fresh_candidate(&resident);
        let (attempt, _) = prepare_cold_successor_v1(
            &seam,
            &Signer,
            &first,
            &mut harness.journal,
            lease(&seam, &first).await,
        )
        .await
        .unwrap();
        stage(&state, &attempt);
        let proof = seal(&state, &resident, &attempt);
        assert_eq!(
            provision_successor_v1(&seam, &Signer, &first, &attempt, &proof)
                .await
                .unwrap(),
            SuccessorStep::KeyReady
        );
        let cancel_hash = B256::repeat_byte(0x99);
        state
            .lock()
            .unwrap()
            .candidate
            .as_mut()
            .unwrap()
            .context_hash = cancel_hash;
        let at = seam.finalized_checkpoint().await.unwrap();
        let view = seam.registry_state(at, &selector()).await.unwrap();
        assert_eq!(
            reconcile_candidate_v1(&view, Some(&attempt), Some(&first)),
            SuccessorStep::CancelRequired {
                context_hash: cancel_hash,
            }
        );
        bump_nonce(&state, &harness.journal);
        cancel_successor_v1(&seam, &Signer, &resident, &mut harness.journal, cancel_hash)
            .await
            .unwrap();
        state.lock().unwrap().candidate = None;
        bump_nonce(&state, &harness.journal);
        let second = fresh_candidate(&resident);
        let (attempt, _) = prepare_cold_successor_v1(
            &seam,
            &Signer,
            &second,
            &mut harness.journal,
            lease(&seam, &second).await,
        )
        .await
        .unwrap();
        stage(&state, &attempt);
        let proof = seal(&state, &resident, &attempt);
        assert_eq!(
            provision_successor_v1(&seam, &Signer, &second, &attempt, &proof)
                .await
                .unwrap(),
            SuccessorStep::KeyReady
        );
        assert!(second.offer_key_ready().unwrap());
        bump_nonce(&state, &harness.journal);
        transition_successor_v1(&seam, &Signer, &second, &mut harness.journal, &attempt)
            .await
            .unwrap();
        let intent = transition_intent(&harness.journal);
        assert_eq!(
            intent.operation,
            AttestationOperationV1::TransitionEnclaveMeasurement
        );
        assert_ne!(intent.enclave_id, first.manifest().enclave_id().unwrap());
        let binding = promoted(source, &intent);
        let mut at = harness.chain.at;
        at.height += 4;
        at.timestamp += 4;
        at.hash = B256::repeat_byte(0x44);
        {
            let mut overlay = state.lock().unwrap();
            overlay.binding = Some(binding.clone());
            overlay.candidate = None;
            overlay.checkpoint = Some(at);
        }
        assert!(at.height > admitted_height);
        assert_eq!(
            reconcile_successor_v1(&seam, &Signer, &second, &mut harness.journal)
                .await
                .unwrap(),
            SuccessorStep::Finalized
        );
        (binding, at, second, intent.enclave_id)
    };
    assert_eq!(binding.enclave_id, second_enclave);
    assert_eq!(second.manifest().enclave_id().unwrap(), second_enclave);
    assert!(second.offer_key_ready().unwrap());
    assert_eq!(state.lock().unwrap().checkpoint, Some(finalized_at));
}
