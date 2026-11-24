use std::sync::Arc;

use super::{context, gratis_request, offer_request, prepared, ClearWitnessPolicy, CHAIN};
use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::{
    pause_account_query_for_test, try_derive_sealed_account_keys, Ledger, TeeError,
};
use outbe_execution::tee_canary::CanaryTickOutcome;
use outbe_gratis::enclave_client::apply_gratis_op;
use outbe_runtime::OfferCanaryProbe;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::{tee_authority_test_guard, TeeAuthorityChannel, TransportError};

fn owner_signature(account: Address, ephemeral: B256) -> [u8; 65] {
    let signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([0x55; 32]).expect("signer");
    let message = outbe_core_common::confidential_tee::derive_account_keys_message(
        Ledger::Gratis,
        account,
        ephemeral,
    );
    let hash = outbe_core_common::confidential_tee::eip191_hash(&message);
    signer.sign_hash(&hash).expect("sign")
}

#[tokio::test]
async fn account_query_and_canary_open_only_with_an_active_resident_key() {
    let _port = tee_authority_test_guard();
    let authority = TeeAuthorityChannel::default();
    outbe_tee::install_tee_authority_watch(authority.subscribe()).expect("install authority");
    assert!(!outbe_tee::current_tee_authority_state().is_authority_open());

    let owner = outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([0x55; 32])
        .expect("signer")
        .address();
    let ephemeral = B256::repeat_byte(0x33);
    let signature = owner_signature(owner, ephemeral);
    let cold = context(6, false);
    assert!(matches!(
        OfferCanaryProbe::bound_canary_decrypt(&cold, &[0x44; 32]),
        CanaryTickOutcome::Failure {
            unreachable: true,
            ..
        }
    ));

    let hot = context(6, true);
    let _hot_runtime = set_in_process_tee_context_for_test(Some(Arc::clone(&hot)));
    let offer_pub = hot.network_encryption_public().expect("offer public");
    assert!(matches!(
        OfferCanaryProbe::bound_canary_decrypt(&hot, &offer_pub),
        CanaryTickOutcome::Success { .. }
    ));

    let closed = account_query(None, owner, ephemeral, signature)
        .await
        .expect_err("closed authority");
    assert!(
        closed.to_string().contains("PrivateExecutionUnavailable"),
        "{closed:?}"
    );
    authority.publish(outbe_tee::TeeAuthorityState::SuspendedJailed);
    let jailed = account_query(Some(authority.subscribe()), owner, ephemeral, signature)
        .await
        .expect_err("jailed authority");
    assert!(
        jailed.to_string().contains("PrivateExecutionUnavailable"),
        "{jailed:?}"
    );
    authority.publish(outbe_tee::TeeAuthorityState::Armed {
        enclave_id: B256::repeat_byte(7),
        valid_until: 123,
    });
    let opened = account_query(Some(authority.subscribe()), owner, ephemeral, signature)
        .await
        .expect("armed account-query");
    assert_eq!(opened.nonce.len(), 12);
    assert_eq!(opened.sealed.len(), 80);

    authority.publish(outbe_tee::TeeAuthorityState::SuspendedJailed);
    let still_jailed =
        account_query(Some(authority.subscribe()), owner, ephemeral, signature).await;
    assert!(still_jailed.is_err());
    // The lease gate closes account-query. The resident key stays installed,
    // so consensus execution and the canary readiness probe still succeed.
    // BoundCanary does not consult TeeAuthorityState.
    assert!(matches!(
        outbe_runtime::OfferCanaryProbe::bound_canary_decrypt(&hot, &offer_pub),
        CanaryTickOutcome::Success { .. }
    ));
    let ready = prepared();
    let _ready_runtime = set_in_process_tee_context_for_test(Some(Arc::clone(&ready.context)));
    apply_gratis_op(ready.gratis).expect("consensus gratis while jailed");
}

#[test]
fn generic_account_key_helper_rejects_a_foreign_ledger_and_a_substituted_pubkey() {
    let _policy = ClearWitnessPolicy;
    outbe_tee::witness::set_witness_policy_for_test(None);
    let hot = context(5, true);
    let _runtime = set_in_process_tee_context_for_test(Some(hot));
    let signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([0x55; 32]).expect("signer");
    let owner = signer.address();
    let pubkey = [0x33; 32];
    let gratis = sign_derive(Ledger::Gratis, owner, pubkey, &signer);

    let Some(foreign) = try_derive_sealed_account_keys(Ledger::Promis, owner, pubkey, gratis)
    else {
        panic!("resident runtime was not installed");
    };
    assert_sealed_account_refusal(foreign.expect_err("Gratis signature on a Promis ledger"));

    let mut substituted = pubkey;
    substituted[0] ^= 1;
    let Some(recipient) =
        try_derive_sealed_account_keys(Ledger::Gratis, owner, substituted, gratis)
    else {
        panic!("resident runtime was not installed");
    };
    assert_sealed_account_refusal(recipient.expect_err("substituted requester pubkey"));
}

#[test]
fn blocked_account_query_does_not_block_consensus() {
    use std::sync::{mpsc, Barrier, OnceLock};
    static GATES: OnceLock<(Barrier, Barrier)> = OnceLock::new();
    fn hold() {
        let gates = GATES.get().expect("account-query gates");
        gates.0.wait();
        gates.1.wait();
    }
    let gates = GATES.get_or_init(|| (Barrier::new(2), Barrier::new(2)));
    let release = &gates.1;
    let _policy = ClearWitnessPolicy;
    outbe_tee::witness::set_witness_policy_for_test(None);
    let hot = context(6, true);
    let request = gratis_request(Vec::new(), 0);
    let (completed, completion) = mpsc::channel();
    std::thread::scope(|scope| {
        let query_context = Arc::clone(&hot);
        let query = scope.spawn(move || {
            let _runtime = set_in_process_tee_context_for_test(Some(query_context));
            let _pause = pause_account_query_for_test(hold);
            try_derive_sealed_account_keys(
                Ledger::Gratis,
                Address::repeat_byte(2),
                [3; 32],
                [0; 65],
            )
        });
        gates.0.wait();
        let consensus_context = Arc::clone(&hot);
        scope.spawn(move || {
            let _runtime = set_in_process_tee_context_for_test(Some(consensus_context));
            let offers = outbe_nod::nod_host::issue_nod_at_offer(offer_request(CHAIN));
            completed
                .send((offers, apply_gratis_op(request)))
                .expect("consensus result");
        });
        let done = completion.recv_timeout(std::time::Duration::from_secs(5));
        release.wait();
        let (offers, gratis) = done.expect("consensus finished during account export");
        assert!(offers.is_ok() && gratis.is_ok());
        assert_sealed_account_refusal(query.join().unwrap().unwrap().unwrap_err());
    });
}

fn assert_sealed_account_refusal(error: TransportError) {
    use std::error::Error;
    assert!(!error.is_node_local(), "{error}");
    assert!(error.source().is_some(), "{error}");
    let TransportError::EnclaveCause { source } = &error else {
        panic!("expected EnclaveCause, got {error}");
    };
    let TeeError::Resident(resident) = source.downcast_ref::<TeeError>().expect("TeeError source")
    else {
        panic!("expected a resident owner refusal, got {source}");
    };
    assert!(
        matches!(
            resident,
            outbe_tee::resident::TeeCryptoError::WrongOwner
                | outbe_tee::resident::TeeCryptoError::OwnerSignature(_)
        ),
        "{resident}"
    );
}

fn sign_derive(
    ledger: Ledger,
    account: Address,
    pubkey: [u8; 32],
    signer: &outbe_primitives::signer::OutbeEvmSigner,
) -> [u8; 65] {
    let message = outbe_core_common::confidential_tee::derive_account_keys_message(
        ledger,
        account,
        B256::from(pubkey),
    );
    let hash = outbe_core_common::confidential_tee::eip191_hash(&message);
    signer.sign_hash(&hash).expect("sign derive-keys message")
}

async fn account_query(
    watch: Option<outbe_tee::authority::TeeAuthorityWatch>,
    owner: Address,
    ephemeral: B256,
    signature: [u8; 65],
) -> jsonrpsee::core::RpcResult<outbe_runtime::domain_rpc::GratisKeysSealed> {
    outbe_runtime::domain_rpc::account_query_through_authority_gate(
        watch,
        Ledger::Gratis,
        owner,
        ephemeral,
        signature.into(),
    )
    .await
}
