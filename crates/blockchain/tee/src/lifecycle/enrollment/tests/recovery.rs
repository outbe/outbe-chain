use super::*;

fn persist_noise(h: &Harness) {
    use std::os::unix::fs::PermissionsExt;
    let host = h
        .dir
        .path()
        .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    std::fs::create_dir(&host).unwrap();
    let path = host.join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1);
    std::fs::write(&path, [4; 32]).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn refresh_admission(h: &mut Harness, fixture: &mut fixtures::AdmissionFixture) {
    use commonware_consensus::types::Epoch;
    use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
    use outbe_tee::finalized_admission::MptStorageProofV1;
    let intent = h.pending_intent();
    let ctx = fixtures::onboarding_context(&intent, h.chain.public.0);
    let lease_slot = outbe_tee::finalized_admission::onboarding_registry_slots_v1(&ctx)[7];
    let leaves = fixture
        .proof
        .registry_storage
        .iter()
        .map(|p| {
            (
                U256::from_be_bytes(p.key.0),
                if p.key == lease_slot {
                    U256::from(intent.requested_valid_until)
                } else {
                    p.value
                },
            )
        })
        .collect::<Vec<_>>();
    let (storage_root, proofs) = storage_trie(&leaves);
    fixture.proof.registry_storage = leaves
        .iter()
        .zip(proofs)
        .map(|((slot, value), nodes)| MptStorageProofV1 {
            key: B256::from(slot.to_be_bytes::<32>()),
            value: *value,
            nodes: nodes.into_iter().map(|v| v.to_vec()).collect(),
        })
        .collect();
    let account = &mut fixture.proof.registry_account;
    account.storage_root = storage_root;
    let (state_root, accounts) = account_mpt_with_proofs(&[(
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
        reth_trie::TrieAccount {
            nonce: account.nonce,
            balance: account.balance,
            storage_root,
            code_hash: account.code_hash,
        },
    )]);
    account.nodes = accounts[&outbe_primitives::addresses::TEE_REGISTRY_ADDRESS]
        .iter()
        .map(|v| v.to_vec())
        .collect();
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let successor = finality::FinalityCommitteeFixture::new(50);
    fixture.anchor = genesis.outcome(Epoch::new(0));
    let transition = genesis.certify_block(
        Epoch::new(0),
        1,
        900,
        B256::repeat_byte(0x81),
        successor.preannounce_extra_data(Epoch::new(1)),
    );
    fixture.transitions = vec![compact(&transition).encode_canonical().unwrap()];
    let certified = successor.certify_block(Epoch::new(1), 20, 2500, state_root, Vec::new());
    fixture.proof.admission = compact(&certified);
    let proof = &mut h.chain.proof.as_mut().unwrap().proof;
    proof.anchor_outcome = fixture.anchor.clone().into();
    proof.committee_transitions = fixture
        .transitions
        .iter()
        .cloned()
        .map(Into::into)
        .collect();
    proof.admission = fixture.proof.encode_canonical().unwrap().into();
    h.chain.at.height = 21;
    h.chain.at.timestamp = 2501;
}

fn compact(
    block: &finality::CertifiedBlockFixture,
) -> outbe_tee::finalized_admission::CertifiedHeaderV1 {
    let decoded = outbe_simplex::committee_chain::decode_public_finalized_block(
        &block.finalization,
        &block.block,
        outbe_simplex::bls::MAX_COMMITTEE_MEMBERS,
    )
    .unwrap();
    outbe_tee::finalized_admission::CertifiedHeaderV1 {
        finalization: block.finalization.clone(),
        header: alloy_rlp::encode(decoded.block.header()),
        descendants: Vec::new(),
    }
}

#[tokio::test]
async fn own_expired_lease_rejoins_and_readmits_the_same_resident_key() {
    let mut h = Harness::new();
    persist_noise(&h);
    h.step().await.unwrap();
    install_finalized_proof(&mut h);
    let first = h.step().await.unwrap();
    let EnrollmentOutcome::Admitted {
        anchor: first_anchor,
    } = first
    else {
        panic!("expected admission")
    };
    let public = h.live.network_encryption_public().unwrap();
    let enclave = h.live.manifest().enclave_id().unwrap();
    h.chain.at.height += 1;
    h.chain.at.timestamp = h.config.valid_until;
    h.config.valid_until = 4000;
    h.config.binding_id = B256::repeat_byte(0x43);
    h.chain.nonces = [1; 3];
    h.chain.jailed = true;
    assert_eq!(h.step().await.unwrap(), EnrollmentOutcome::WaitForUnjail);
    h.chain.jailed = false;
    h.chain.balance = U256::ZERO;
    assert_eq!(
        h.step().await.unwrap(),
        EnrollmentOutcome::WaitForGasBalance
    );
    h.chain.balance = U256::MAX;
    assert!(matches!(
        h.step().await.unwrap(),
        EnrollmentOutcome::Submitted { .. }
    ));
    let intent = h.pending_intent();
    assert_eq!(
        (intent.binding_version, intent.registration_version),
        (2, 1)
    );
    assert_eq!(intent.enclave_id, enclave);
    assert_eq!(h.live.network_encryption_public().unwrap(), public);
    let mut fixture = install_finalized_proof(&mut h);
    refresh_admission(&mut h, &mut fixture);
    let outcome = h.step().await.unwrap();
    let EnrollmentOutcome::Admitted { anchor } = &outcome else {
        panic!("expected renewed admission")
    };
    assert!(anchor.finalized_height > first_anchor.finalized_height);
    assert_ne!(anchor.intent_hash, first_anchor.intent_hash);
    assert_eq!(h.chain.binding.as_ref().unwrap().valid_until, 4000);
    assert_eq!(
        load_live_join_admission_anchor_v1(h.dir.path(), &h.live).unwrap(),
        Some(**anchor)
    );
    assert_eq!(h.live.network_encryption_public().unwrap(), public);
    assert_eq!(h.step().await.unwrap(), outcome);
    assert_eq!(h.chain.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn jailed_upgrade_waits_for_its_activation_height() {
    let mut h = Harness::new();
    h.chain.jailed = true;
    h.chain.strict = true;
    h.chain.upgrade_height = h.chain.at.height + 1;
    assert_eq!(h.step().await.unwrap(), EnrollmentOutcome::WaitForUnjail);
    h.chain.at.height += 1;
    assert_eq!(
        h.step().await.unwrap(),
        EnrollmentOutcome::RequiresStrictUpgradeLateRecovery
    );
    assert!(h.chain.sent.lock().unwrap().is_empty());
    assert!(h.journal.load().unwrap().is_none());
}

#[tokio::test]
async fn expired_renew_rebuild_rejoins_with_a_fresh_register_and_pending_renew_blocks_it() {
    let mut h = Harness::new();
    let renew_until = h.chain.at.timestamp + h.chain.policy.maximum_lease / 2;
    bind_resident(&mut h, renew_until);
    let submitted = outbe_tee::lifecycle::renewal::renew_once_v1(
        &h.chain,
        &Signer,
        &h.live,
        &mut h.journal,
        outbe_tee::lifecycle::renewal::RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    let renew = h.journal.load().unwrap().unwrap();
    assert!(matches!(
        submitted,
        outbe_tee::lifecycle::renewal::RenewalStep::Submitted {
            transaction_hash,
            replayed: false
        } if transaction_hash == renew.tx_hash
    ));
    assert_eq!(renew.kind, ServiceTxKindV1::Renew);
    assert_eq!(renew.state, ServiceTxStateV1::Pending);
    assert_eq!(call_name(&renew), "renew");

    h.chain.binding.as_mut().unwrap().enclave_id = B256::repeat_byte(0xab);
    assert!(matches!(
        h.step().await,
        Err(EnrollmentError::WrongServiceTransaction)
    ));
    assert_eq!(h.journal.load().unwrap().unwrap(), renew);
    assert_eq!(h.chain.sent.lock().unwrap().len(), 1);

    h.chain.at.timestamp = renew_until;
    h.config.valid_until = renew_until + h.chain.policy.maximum_lease;
    assert_eq!(
        reconcile_service_tx_v1(&h.chain, &mut h.journal, &h.live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign { nonce: renew.nonce }
    );
    let rebuilt = h.journal.load().unwrap().unwrap();
    assert_eq!(rebuilt.kind, ServiceTxKindV1::Renew);
    assert_eq!(rebuilt.state, ServiceTxStateV1::RebuildRequired);
    assert_eq!(rebuilt.tx_hash, renew.tx_hash);
    assert_eq!(call_name(&rebuilt), "renew");

    assert!(matches!(
        h.step().await.unwrap(),
        EnrollmentOutcome::Submitted { .. }
    ));
    let rejoined = h.journal.load().unwrap().unwrap();
    assert_eq!(rejoined.kind, ServiceTxKindV1::Register);
    assert_eq!(rejoined.state, ServiceTxStateV1::Pending);
    assert_eq!(call_name(&rejoined), "register");
    assert_ne!(rejoined.tx_hash, renew.tx_hash);
    let intent = h.pending_intent();
    assert_eq!(
        (intent.binding_version, intent.registration_version),
        (5, 4)
    );
    assert_eq!(h.chain.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn rebuild_required_upgrade_cancel_rejoins_and_pending_cancel_blocks_register() {
    let mut h = Harness::new();
    let pending = signed_cancel(&h);
    h.journal.begin(pending.clone()).unwrap();
    assert_eq!(call_name(&pending), "cancel");
    assert!(matches!(
        h.step().await,
        Err(EnrollmentError::WrongServiceTransaction)
    ));
    assert_eq!(h.journal.load().unwrap().unwrap(), pending);
    assert!(h.chain.sent.lock().unwrap().is_empty());

    h.chain.nonces = [pending.nonce + 1; 3];
    assert_eq!(
        reconcile_service_tx_v1(&h.chain, &mut h.journal, &h.live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign {
            nonce: pending.nonce + 1
        }
    );
    let rebuilt = h.journal.load().unwrap().unwrap();
    assert_eq!(rebuilt.kind, ServiceTxKindV1::UpgradeCancel);
    assert_eq!(rebuilt.state, ServiceTxStateV1::RebuildRequired);
    assert_eq!(rebuilt.tx_hash, pending.tx_hash);
    assert_eq!(call_name(&rebuilt), "cancel");

    h.config.valid_until = h.chain.at.timestamp + h.chain.policy.minimum_lease;
    assert!(matches!(
        h.step().await.unwrap(),
        EnrollmentOutcome::Submitted { .. }
    ));
    let rejoined = h.journal.load().unwrap().unwrap();
    assert_eq!(rejoined.kind, ServiceTxKindV1::Register);
    assert_eq!(rejoined.state, ServiceTxStateV1::Pending);
    assert_eq!(call_name(&rejoined), "register");
    assert_ne!(rejoined.tx_hash, pending.tx_hash);
    assert_eq!(rejoined.nonce, pending.nonce + 1);
    assert_eq!(h.chain.sent.lock().unwrap().len(), 1);
}

fn bind_resident(h: &mut Harness, valid_until: u64) {
    let manifest = h.live.manifest();
    let mut binding = outbe_tee::test_utils::RenewalBindingFixtureV1::new(20)
        .versions(4, 3)
        .lease(900, valid_until, valid_until + 1_000)
        .build();
    binding.node_id_hash = manifest.node_id.node_id_hash().unwrap();
    binding.enclave_id = manifest.enclave_id().unwrap();
    binding.node_host_authorization_hash = manifest.node_host_authorization_hash().unwrap();
    binding.policy_hash = h.chain.policy.policy_hash().unwrap();
    binding.recipient_x25519 = manifest.recipient_x25519.into();
    binding.attestation_ed25519 = manifest.attestation_ed25519.into();
    binding.noise_responder_x25519 = manifest.noise_responder_x25519.into();
    h.chain.binding = Some(binding);
}

fn signed_cancel(h: &Harness) -> ServiceTxRecordV1 {
    use alloy_consensus::{SignableTransaction, TxEip1559};
    use alloy_primitives::{keccak256, TxKind, U256};
    use alloy_sol_types::SolCall as _;
    let manifest = h.live.manifest();
    let target = ServiceBindingTargetV1 {
        node_id_hash: manifest.node_id.node_id_hash().unwrap(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: h.config.binding_id,
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        valid_until: h.chain.at.timestamp + 1,
        policy_hash: h.chain.policy.policy_hash().unwrap(),
        recipient_x25519: manifest.recipient_x25519.into(),
    };
    let candidate = LifecycleUpgradeCandidateV1 {
        context_hash: B256::repeat_byte(0x21),
        valid_until: target.valid_until,
        source_binding_id: target.binding_id,
        target_hash: B256::repeat_byte(0x22),
        nonce: 1,
    };
    let calldata = ITeeRegistryV1::cancelEnclaveUpgradeCall {
        nodeIdHash: target.node_id_hash,
        expectedContextHash: candidate.context_hash,
    }
    .abi_encode();
    let tx = TxEip1559 {
        chain_id: h.chain.at.chain_id,
        nonce: 0,
        gas_limit: 100_000,
        max_fee_per_gas: 7,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS),
        value: U256::ZERO,
        input: calldata.into(),
        access_list: Default::default(),
    };
    let signature = EnrollmentSignerV1::sign_operator_hash(&Signer, tx.signature_hash()).unwrap();
    let mut raw = vec![2];
    tx.into_signed(signature).rlp_encode(&mut raw);
    let record = ServiceTxRecordV1 {
        tx_hash: keccak256(&raw),
        raw_signed_tx: raw.into(),
        sender: Signer::address(),
        nonce: 0,
        kind: ServiceTxKindV1::UpgradeCancel,
        public_intent: ServiceTxIntentV1 {
            target,
            candidate: Some(candidate),
        },
        finalized_checkpoint: h.chain.at,
        state: ServiceTxStateV1::Pending,
        reconciled_checkpoint: None,
    };
    record.validate().unwrap();
    record
}

fn call_name(record: &ServiceTxRecordV1) -> &'static str {
    use alloy_consensus::transaction::RlpEcdsaDecodableTx as _;
    use alloy_sol_types::SolCall as _;
    let tx =
        alloy_consensus::TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
    let input = tx.tx().input.as_ref();
    if ITeeRegistryV1::renewEnclaveCall::abi_decode(input).is_ok() {
        "renew"
    } else if ITeeRegistryV1::registerEnclaveCall::abi_decode(input).is_ok() {
        "register"
    } else if ITeeRegistryV1::cancelEnclaveUpgradeCall::abi_decode(input).is_ok() {
        "cancel"
    } else {
        "other"
    }
}

impl outbe_tee::lifecycle::renewal::LifecycleSignerV1 for Signer {
    type Error = std::convert::Infallible;
    fn address(&self) -> Address {
        Signer::address()
    }
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error> {
        EnrollmentSignerV1::sign_node_hash(self, hash)
    }
    fn sign_transaction_hash(
        &self,
        hash: B256,
    ) -> Result<alloy_primitives::Signature, Self::Error> {
        EnrollmentSignerV1::sign_operator_hash(self, hash)
    }
}

#[tokio::test]
async fn unfunded_dcap_initial_and_expired_join_do_not_request_hardware_evidence() {
    for (expired, balance) in [
        (false, U256::ZERO),
        (true, U256::ZERO),
        (false, U256::from(1)),
        (true, U256::from(1)),
    ] {
        let mut h = Harness::new();
        h.live = context(11, AttestationMode::DcapRequired);
        h.chain.policy.attestation_mode = AttestationMode::DcapRequired;
        if expired {
            h.predecessor(1000)
        }
        h.chain.balance = balance;
        assert_eq!(
            h.step().await.unwrap(),
            EnrollmentOutcome::WaitForGasBalance
        );
        assert!(h.journal.load().unwrap().is_none());
        assert!(h.chain.sent.lock().unwrap().is_empty());
    }
}
