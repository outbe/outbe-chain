use super::{
    execution_committee::Committee,
    proof,
    rejoin_execution::{sign_unjail, Ledger},
    Harness,
};
use alloy_consensus::{transaction::RlpEcdsaDecodableTx, TxEip1559};
use alloy_primitives::{B256, U256};
use outbe_runtime::LifecycleSupervisorProbe;
use outbe_tee::lifecycle::{
    enrollment::EnrollmentOutcome, renewal::RenewalStep, ServiceTxKindV1, ServiceTxStateV1,
    VALIDATOR_STATUS_JAILED,
};
use std::sync::{Arc, Mutex};

#[path = "rejoin_rpc.rs"]
mod rejoin_rpc;
use rejoin_rpc::RejoinChain;

#[tokio::test]
async fn expired_renew_waits_for_unjail_nonce_then_pays_for_rejoin() {
    outbe_consensus::config::init_consensus_chain_id(super::TEST_CHAIN_ID).unwrap();
    let mut h = Harness::new();
    h.chain.at.height = 0;
    h.chain.at.hash = h.chain.at.genesis_hash;
    let _donor = install_donor(&mut h);
    let ledger = Ledger::new(&h, h.chain.public);
    let dir = tempfile::tempdir().unwrap();
    super::provision_host_key(dir.path());
    let chain = Arc::new(Mutex::new(RejoinChain::new(
        ledger,
        Committee::new(10),
        dir.path().to_path_buf(),
    )));
    let descriptor = proof::descriptor(&super::finality::FinalityCommitteeFixture::new(10));
    let mut supervisor = LifecycleSupervisorProbe::new(
        rejoin_rpc::methods(chain.clone()),
        super::test_signers(),
        (h.live.clone(), dir.path().to_path_buf(), descriptor),
    )
    .unwrap();
    assert!(matches!(
        supervisor.observe_enrollment().await.unwrap(),
        Some(EnrollmentOutcome::Submitted { .. })
    ));
    finalize_paid_register(&mut supervisor, &chain).await;
    let old = {
        let mut chain = chain.lock().unwrap();
        let binding = chain.latest.binding();
        chain.advance(binding.valid_until - 3600, None);
        chain.finalize();
        binding
    };

    assert!(matches!(
        supervisor.observe_renewal().await.unwrap(),
        Some(RenewalStep::Submitted {
            replayed: false,
            ..
        })
    ));
    let renew = chain.lock().unwrap().record();
    assert_eq!(renew.kind, ServiceTxKindV1::Renew);
    assert_eq!(renew.state, ServiceTxStateV1::Pending);
    let latest_nonce = mine_unjail_without_finalizing(&chain, old.valid_until, renew.nonce);

    assert_eq!(
        supervisor.observe_renewal().await.unwrap(),
        Some(RenewalStep::AwaitingFinality)
    );
    let awaiting = chain.lock().unwrap().record();
    assert_eq!(awaiting.tx_hash, renew.tx_hash);
    assert_eq!(awaiting.state, ServiceTxStateV1::Pending);
    {
        let mut chain = chain.lock().unwrap();
        assert_eq!(
            chain.sent.len(),
            2,
            "no paid rejoin before unjail is finalized"
        );
        chain.finalize();
    }

    assert!(matches!(
        supervisor.observe_enrollment().await.unwrap(),
        Some(EnrollmentOutcome::Submitted { .. })
    ));
    assert_eq!(chain.lock().unwrap().retired_renewal, Some(renew.tx_hash));
    let register = chain.lock().unwrap().record();
    assert_eq!(register.kind, ServiceTxKindV1::Register);
    assert_eq!(register.nonce, latest_nonce);
    assert_ne!(register.tx_hash, renew.tx_hash);
    let intent = register.public_intent.target;
    assert_eq!(intent.binding_version, old.binding_version + 1);
    assert_ne!(intent.binding_id, old.binding_id);
    finalize_paid_register(&mut supervisor, &chain).await;

    let mut chain = chain.lock().unwrap();
    let binding = chain.finalized.binding();
    assert_eq!(binding.binding_id, intent.binding_id);
    assert_eq!(binding.registration_version, old.registration_version + 1);
    assert_eq!(chain.finalized.nonce_and_balance().0, latest_nonce + 1);
    assert_ne!(chain.finalized.status(), VALIDATOR_STATUS_JAILED);
    assert_eq!(chain.sent.len(), 3);
    assert!(h.live.offer_key_ready().unwrap());
}

fn mine_unjail_without_finalizing(chain: &Mutex<RejoinChain>, expiry: u64, nonce: u64) -> u64 {
    let mut chain = chain.lock().unwrap();
    chain.latest.seed_validator(true);
    chain.advance(expiry, None);
    chain.finalize();
    assert_eq!(chain.finalized.status(), VALIDATOR_STATUS_JAILED);
    assert_eq!(chain.finalized.nonce_and_balance().0, nonce);

    let jailed = chain.finalized.tip;
    let _ = chain
        .advance(jailed.timestamp + 1, Some(&sign_unjail(nonce)))
        .unwrap();
    assert_ne!(chain.latest.status(), VALIDATOR_STATUS_JAILED);
    assert_eq!(chain.finalized.status(), VALIDATOR_STATUS_JAILED);
    assert_ne!(chain.latest.tip.hash, jailed.hash);
    assert_ne!(chain.latest.tip.state_root, jailed.state_root);
    let latest_nonce = chain.latest.nonce_and_balance().0;
    assert_eq!(latest_nonce, nonce + 1);
    latest_nonce
}

async fn finalize_paid_register(
    supervisor: &mut LifecycleSupervisorProbe,
    chain: &Mutex<RejoinChain>,
) {
    let finalized = {
        let mut chain = chain.lock().unwrap();
        let raw = chain.sent.last().unwrap().clone();
        let tx = TxEip1559::rlp_decode_signed(&mut &raw[1..]).unwrap();
        assert!(tx.tx().max_fee_per_gas > 0);
        let (nonce, balance) = chain.latest.nonce_and_balance();
        let previous = chain.latest.tip;
        let receipt = chain.advance(previous.timestamp + 1, Some(&raw)).unwrap();
        assert_eq!(
            chain.latest.nonce_and_balance(),
            (nonce + 1, balance - U256::from(receipt.tx_gas_used()))
        );
        assert_ne!(chain.latest.tip.hash, previous.hash);
        assert_ne!(chain.latest.tip.state_root, previous.state_root);
        chain.finalize();
        chain.finalized.tip
    };
    let step = supervisor.observe_enrollment().await.unwrap();
    let Some(EnrollmentOutcome::Admitted { anchor }) = step else {
        panic!("expected admission, got {step:?}");
    };
    assert_eq!(anchor.finalized_height, finalized.height);
    assert_eq!(anchor.finalized_hash, finalized.hash);
}

pub(super) fn install_donor(h: &mut Harness) -> outbe_tee::in_process_dispatch::InProcessTestGuard {
    let donor = proof::context(
        1,
        &proof::descriptor(&super::finality::FinalityCommitteeFixture::new(10)),
    );
    donor
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(donor.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    h.chain.public = donor.network_encryption_public().unwrap().into();
    outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(donor.into()))
}
