use alloy_primitives::{Address, B256, U256};
use outbe_primitives::tee_attestation_v1::*;
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::lifecycle::{enrollment::*, *};

mod chain;
#[allow(dead_code)]
#[path = "../../../tests/in_process_dkg_and_admission/finality.rs"]
mod finality;
#[allow(dead_code)]
#[path = "../../../tests/in_process_dkg_and_admission/fixtures.rs"]
mod fixtures;
mod recovery;
use chain::{Chain, Signer};

struct Harness {
    dir: tempfile::TempDir,
    journal: ServiceTxJournalV1,
    live: InProcessTeeContext,
    chain: Chain,
    config: EnrollmentConfigV1,
}
impl Harness {
    fn new() -> Self {
        let genesis = finality::FinalityCommitteeFixture::new(10);
        let descriptor = fixtures::AdmissionFixture::descriptor(&genesis);
        let live = context(11, AttestationMode::NoAttestationDev);
        let dir = tempfile::tempdir().unwrap();
        let journal = ServiceTxJournalV1::open(dir.path()).unwrap();
        let at = LifecycleCheckpointV1 {
            height: 10,
            hash: B256::repeat_byte(1),
            state_root: B256::repeat_byte(2),
            timestamp: 1000,
            base_fee_per_gas: U256::from(1),
            chain_id: 676,
            genesis_hash: descriptor.network_binding.genesis_hash,
        };
        let chain = Chain {
            at,
            binding: None,
            policy: outbe_primitives::tee_test_utils::no_attestation_dev_policy_v1(
                676,
                at.genesis_hash,
            )
            .unwrap(),
            jailed: false,
            strict: false,
            upgrade_height: 0,
            balance: U256::MAX,
            nonces: [0; 3],
            sent: Default::default(),
            proof: None,
            public: B256::repeat_byte(9),
            fail_nonce: false,
            journal_dir: dir.path().to_path_buf(),
        };
        Self {
            dir,
            journal,
            live,
            chain,
            config: EnrollmentConfigV1 {
                sender: Signer::address(),
                binding_id: B256::repeat_byte(0x42),
                valid_until: 2000,
                max_fee_per_gas: 2,
                max_priority_fee_per_gas: 1,
            },
        }
    }
    async fn step(
        &mut self,
    ) -> Result<EnrollmentOutcome, EnrollmentError<std::io::Error, std::convert::Infallible>> {
        enroll_step_v1(
            &self.chain,
            &Signer,
            &mut self.journal,
            EnrollmentSessionV1 {
                live: &self.live,
                config: self.config,
                node_data_dir: self.dir.path(),
            },
        )
        .await
    }
    fn pending_intent(&self) -> RegistrationIntentV1 {
        use alloy_consensus::transaction::RlpEcdsaDecodableTx;
        use alloy_sol_types::SolCall;
        let record = self.journal.load().unwrap().unwrap();
        let tx =
            alloy_consensus::TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
        let call =
            outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::registerEnclaveCall::abi_decode(
                &tx.tx().input,
            )
            .unwrap();
        match AttestationEvidenceV1::decode_canonical(&call.evidence).unwrap() {
            AttestationEvidenceV1::NoAttestationDev(v) => v.intent,
            AttestationEvidenceV1::Dcap(v) => v.intent,
        }
    }
    fn predecessor(&mut self, valid_until: u64) {
        let mut b = outbe_tee::test_utils::RenewalBindingFixtureV1::new(20)
            .versions(4, 3)
            .lease(900, valid_until, 4000)
            .build();
        b.node_id_hash = self.live.manifest().node_id.node_id_hash().unwrap();
        self.chain.binding = Some(b);
    }
}

#[tokio::test]
async fn initial_join_before_stake_is_durable_before_send() {
    let mut h = Harness::new();
    assert!(matches!(
        h.step().await.unwrap(),
        EnrollmentOutcome::Submitted { .. }
    ));
    let intent = h.pending_intent();
    assert_eq!(
        (intent.binding_version, intent.registration_version),
        (1, 0)
    );
    assert_eq!(intent.enclave_id, h.live.manifest().enclave_id().unwrap());
    assert_eq!(h.chain.sent.lock().unwrap().len(), 1);
    assert!(!h.live.offer_key_ready().unwrap());
}

#[tokio::test]
async fn jail_strict_upgrade_and_insufficient_gas_wait_without_submission() {
    for (jailed, strict, balance, expected) in [
        (true, false, U256::MAX, EnrollmentOutcome::WaitForUnjail),
        (
            true,
            true,
            U256::MAX,
            EnrollmentOutcome::RequiresStrictUpgradeLateRecovery,
        ),
        (
            false,
            false,
            U256::ZERO,
            EnrollmentOutcome::WaitForGasBalance,
        ),
    ] {
        let mut h = Harness::new();
        h.chain.jailed = jailed;
        h.chain.strict = strict;
        h.chain.balance = balance;
        assert_eq!(h.step().await.unwrap(), expected);
        assert!(h.journal.load().unwrap().is_none());
        assert!(h.chain.sent.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn active_and_expired_rejoin_preserve_counters_and_old_binding_is_not_competition() {
    for valid_until in [2000, 1000] {
        let mut h = Harness::new();
        h.predecessor(valid_until);
        assert!(matches!(
            h.step().await.unwrap(),
            EnrollmentOutcome::Submitted { .. }
        ));
        let intent = h.pending_intent();
        assert_eq!(
            (intent.binding_version, intent.registration_version),
            (5, 4)
        );
        h.chain.nonces = [0, 1, 1];
        assert_eq!(h.step().await.unwrap(), EnrollmentOutcome::WaitForFinality);
        assert_eq!(h.chain.sent.lock().unwrap().len(), 1);
        h.chain.nonces = [0; 3];
        assert!(matches!(
            h.step().await.unwrap(),
            EnrollmentOutcome::Submitted { .. }
        ));
        h.chain.nonces = [0, 1, 1];
        h.chain.binding.as_mut().unwrap().binding_version = 5;
        assert!(matches!(
            h.step().await,
            Err(EnrollmentError::CompetingLiveBinding)
        ));
    }
}

#[tokio::test]
async fn external_nonce_rebuild_and_lost_process_recovery_keep_new_identity() {
    let mut h = Harness::new();
    h.step().await.unwrap();
    let identity = h.live.manifest().enclave_id().unwrap();
    h.chain.nonces = [1, 1, 1];
    h.step().await.unwrap();
    assert_eq!(h.journal.load().unwrap().unwrap().nonce, 1);
    assert_eq!(h.live.manifest().enclave_id().unwrap(), identity);
    let old = h.journal.load().unwrap().unwrap();
    h.live = context(12, AttestationMode::NoAttestationDev);
    drop(h.journal);
    h.journal = ServiceTxJournalV1::open(h.dir.path()).unwrap();
    h.chain.nonces = [1, 2, 2];
    assert_eq!(h.step().await.unwrap(), EnrollmentOutcome::WaitForFinality);
    assert_eq!(h.journal.load().unwrap().unwrap(), old);
    h.chain.nonces = [1, 1, 1];
    h.step().await.unwrap();
    let new = h.journal.load().unwrap().unwrap();
    assert_eq!(new.nonce, old.nonce);
    assert_ne!(
        new.public_intent.target.enclave_id,
        old.public_intent.target.enclave_id
    );
    assert_eq!(
        new.public_intent.target.enclave_id,
        h.live.manifest().enclave_id().unwrap()
    );
}

fn context(seed: u8, mode: AttestationMode) -> InProcessTeeContext {
    use outbe_tee::{in_process::ProofSourceV1, resident::ResidentTeeKeys, NodeHostIdentityV1};
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let mut descriptor = fixtures::AdmissionFixture::descriptor(&genesis);
    descriptor.network_binding.attestation_mode = mode;
    let mut manifest = fixtures::context(seed, &fixtures::AdmissionFixture::descriptor(&genesis))
        .manifest()
        .clone();
    manifest.attestation_mode = mode;
    manifest.node_host_noise_x25519 = outbe_tee::NodeHostNoiseKey::from_private([4; 32]).public();
    struct Hardware;
    impl outbe_tee::in_process::HardwareProofSourceV1 for Hardware {
        fn code_identity(&self) -> outbe_tee::in_process::Result<(B256, B256, u16, u16)> {
            Ok((B256::repeat_byte(1), B256::repeat_byte(2), 1, 1))
        }
        fn quote(&self, _: [u8; 64]) -> outbe_tee::in_process::Result<Vec<u8>> {
            Err(outbe_tee::in_process::InProcessError::HardwareProofUnavailable)
        }
    }
    let proof = if mode == AttestationMode::NoAttestationDev {
        ProofSourceV1::NoHardware
    } else {
        ProofSourceV1::Hardware(std::sync::Arc::new(Hardware))
    };
    let identity = NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    InProcessTeeContext::new(
        ResidentTeeKeys::from_seed([seed; 32]).unwrap(),
        identity,
        manifest,
        descriptor,
        proof,
    )
    .unwrap()
}

#[tokio::test]
async fn dcap_live_rejoin_is_terminal_but_expired_lease_reaches_hardware_source() {
    let mut h = Harness::new();
    h.live = context(11, AttestationMode::DcapRequired);
    h.chain.policy.attestation_mode = AttestationMode::DcapRequired;
    h.predecessor(2000);
    assert!(matches!(
        h.step().await,
        Err(EnrollmentError::LiveRejoinForbiddenInMode {
            mode: AttestationMode::DcapRequired
        })
    ));
    h.chain.binding.as_mut().unwrap().valid_until = 1000;
    assert!(matches!(
        h.step().await,
        Err(EnrollmentError::Resident(
            outbe_tee::in_process::InProcessError::HardwareProofUnavailable
        ))
    ));
    assert!(h.journal.load().unwrap().is_none());
    assert!(h.chain.sent.lock().unwrap().is_empty());
}

fn binding(intent: &RegistrationIntentV1) -> outbe_tee::registry_view::RenewalBindingV1 {
    let mut b = outbe_tee::test_utils::RenewalBindingFixtureV1::new(20).build();
    b.node_id_hash = intent.node_id.node_id_hash().unwrap();
    b.enclave_id = intent.enclave_id;
    b.binding_id = intent.binding_id;
    b.intent_hash = intent.intent_hash().unwrap();
    b.policy_hash = intent.policy_hash;
    b.binding_version = intent.binding_version;
    b.registration_version = intent.registration_version;
    b.renewal_nonce = intent.renewal_nonce;
    b.transition_nonce = intent.transition_nonce;
    b.valid_until = intent.requested_valid_until;
    b.recipient_x25519 = intent.recipient_x25519.into();
    b.attestation_ed25519 = intent.attestation_ed25519.into();
    b.noise_responder_x25519 = intent.noise_responder_x25519.into();
    b.node_host_authorization_hash = intent.node_host_authorization_hash;
    b
}

fn install_finalized_proof(h: &mut Harness) -> fixtures::AdmissionFixture {
    outbe_simplex::config::init_consensus_chain_id(676).unwrap();
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let donor = fixtures::context(1, &fixtures::AdmissionFixture::descriptor(&genesis));
    donor
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(h.live.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let intent = h.pending_intent();
    let ctx = fixtures::onboarding_context(&intent, donor.network_encryption_public().unwrap());
    let artifact = donor
        .prepare_no_attestation_dev_onboarding_artifact_v2(ctx)
        .unwrap()
        .onboarding_artifact;
    let fixture = fixtures::AdmissionFixture::new(&genesis, &ctx, false);
    h.chain.public = ctx.network_encryption_public.into();
    h.chain.binding = Some(binding(&intent));
    h.chain.proof = Some(LifecycleOnboardingProofV1 {
        artifact: artifact.into(),
        proof: outbe_tee::upgrade_transfer::UpgradeKeyProofV1 {
            anchor_outcome: fixture.anchor.clone().into(),
            committee_transitions: fixture
                .transitions
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            admission: fixture.proof.encode_canonical().unwrap().into(),
        },
    });
    h.chain.at.height += 1;
    h.chain.at.timestamp += 1;
    fixture
}

#[tokio::test]
async fn finalized_registration_then_jail_ingests_without_resubmitting_and_rejects_stale_anchor() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Harness::new();
    h.step().await.unwrap();
    install_finalized_proof(&mut h);
    h.chain.jailed = true;
    h.chain.balance = U256::ZERO;
    let host = h
        .dir
        .path()
        .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    std::fs::create_dir(&host).unwrap();
    std::fs::write(
        host.join(outbe_tee::node_host::NODE_HOST_NOISE_KEY_V1),
        [4; 32],
    )
    .unwrap();
    std::fs::set_permissions(
        host.join(outbe_tee::node_host::NODE_HOST_NOISE_KEY_V1),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let outcome = h.step().await.unwrap();
    let EnrollmentOutcome::Admitted { anchor } = &outcome else {
        panic!("expected admitted")
    };
    assert_eq!(h.chain.sent.lock().unwrap().len(), 1);
    assert!(h.live.offer_key_ready().unwrap());
    assert_eq!(
        h.live.network_encryption_public().unwrap(),
        h.chain.public.0
    );
    assert_eq!(
        load_live_join_admission_anchor_v1(h.dir.path(), &h.live).unwrap(),
        Some(**anchor)
    );
    assert_eq!(h.step().await.unwrap(), outcome);
    let path = h.dir.path().join("tee-live-join-admission-v1/anchor.bin");
    assert_eq!(std::fs::read(&path).unwrap().len(), 241);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    h.live = context(12, AttestationMode::NoAttestationDev);
    assert!(matches!(
        load_live_join_admission_anchor_v1(h.dir.path(), &h.live),
        Err(LiveJoinAnchorError::IdentityMismatch)
    ));
    assert!(!h.live.offer_key_ready().unwrap());
}

#[tokio::test]
async fn forged_proof_never_activates_or_persists_an_anchor() {
    let mut h = Harness::new();
    h.step().await.unwrap();
    let mut fixture = install_finalized_proof(&mut h);
    fixture.proof.registry_account.nodes[0][0] ^= 1;
    h.chain.proof.as_mut().unwrap().proof.admission =
        fixture.proof.encode_canonical().unwrap().into();
    assert!(matches!(h.step().await, Err(EnrollmentError::Resident(_))));
    assert!(!h.live.offer_key_ready().unwrap());
    assert!(!h
        .dir
        .path()
        .join("tee-live-join-admission-v1/anchor.bin")
        .exists());
    assert_eq!(h.chain.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn replay_also_preflights_the_exact_signed_gas_budget() {
    use alloy_consensus::transaction::RlpEcdsaDecodableTx;
    let mut h = Harness::new();
    h.step().await.unwrap();
    let record = h.journal.load().unwrap().unwrap();
    let signed =
        alloy_consensus::TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
    let budget = U256::from(signed.tx().gas_limit) * U256::from(signed.tx().max_fee_per_gas);
    h.chain.balance = budget - U256::from(1);
    assert_eq!(
        h.step().await.unwrap(),
        EnrollmentOutcome::WaitForGasBalance
    );
    assert_eq!(h.chain.sent.lock().unwrap().len(), 1);
    assert_eq!(h.journal.load().unwrap().unwrap(), record);
    h.chain.balance = budget;
    assert!(matches!(
        h.step().await.unwrap(),
        EnrollmentOutcome::Submitted { .. }
    ));
}

#[tokio::test]
async fn finalized_lost_registration_is_not_replayed_for_the_new_process() {
    let mut h = Harness::new();
    h.step().await.unwrap();
    let old = h.pending_intent();
    h.chain.binding = Some(binding(&old));
    h.live = context(12, AttestationMode::NoAttestationDev);
    drop(h.journal);
    h.journal = ServiceTxJournalV1::open(h.dir.path()).unwrap();
    h.chain.at.height += 1;
    h.chain.at.timestamp += 1;
    h.chain.nonces = [1; 3];
    assert!(matches!(
        h.step().await.unwrap(),
        EnrollmentOutcome::Submitted { .. }
    ));
    let new = h.pending_intent();
    assert_eq!((new.binding_version, new.registration_version), (2, 1));
    assert_ne!(new.enclave_id, old.enclave_id);
    assert_eq!(h.chain.sent.lock().unwrap().len(), 2);
    assert!(!h.live.offer_key_ready().unwrap());
}

#[tokio::test]
async fn nonce_send_failure_is_reconciled_without_erasing_context() {
    let mut h = Harness::new();
    h.chain.fail_nonce = true;
    assert_eq!(h.step().await.unwrap(), EnrollmentOutcome::WaitForFinality);
    let old = h.journal.load().unwrap().unwrap();
    h.chain.nonces = [1; 3];
    h.chain.fail_nonce = false;
    assert!(matches!(
        h.step().await.unwrap(),
        EnrollmentOutcome::Submitted { .. }
    ));
    let new = h.journal.load().unwrap().unwrap();
    assert_eq!(new.nonce, 1);
    assert_ne!(old.tx_hash, new.tx_hash);
    assert_eq!(old.public_intent, new.public_intent);
}
