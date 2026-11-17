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
