use super::*;
use crate::protocol::{EnclaveRequest, EnclaveResponse};
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::*;
use outbe_tee::{in_process::*, resident::ResidentTeeKeys, NodeHostIdentityV1};
use std::sync::Arc;

pub(crate) fn context(seed: u8) -> Arc<InProcessTeeContext> {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let node = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: alloy_primitives::U256::from(777u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id: NodeIdV1 {
            reth_p2p_public: node
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let identity = NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: Vec::new(),
    };
    Arc::new(
        InProcessTeeContext::new(
            keys,
            identity,
            manifest,
            descriptor,
            ProofSourceV1::NoHardware,
        )
        .unwrap(),
    )
}

#[test]
fn in_process_lanes_reject_mutating_account_queries_and_canary_without_state_change() {
    let context = context(7);
    context
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let before = context.network_encryption_public().unwrap();
    let _runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some((
        Arc::clone(&context),
        dispatch_payload,
    )));
    let request = EnclaveRequest::ProcessTributeOfferBatch { offers: Vec::new() };
    let mutation = EnclaveRequest::ApplyPromisOp {
        request: Box::new(crate::protocol::PromisOpRequest {
            op: crate::protocol::PromisOp::Mint,
            chain_id: B256::from(context.manifest().chain_id),
            account: alloy_primitives::Address::repeat_byte(2),
            amount: alloy_primitives::U256::ONE,
            current_balance: Vec::new(),
            modify_auth: crate::protocol::ModifyAuth {
                mac: [0; 32],
                op_nonce: 0,
            },
        }),
    };
    let payload = postcard::to_allocvec(&mutation).unwrap();
    let forbidden = outbe_tee::protocol::EnclaveRequest::Payload {
        lane: outbe_tee::protocol::PayloadLane::AccountQuery,
        payload,
    };
    assert!(matches!(
        outbe_tee::try_account_query(&forbidden).unwrap(),
        Err(outbe_tee::TransportError::InProcess(_))
    ));
    assert!(matches!(
        crate::host::canary_domain_request(&mutation),
        Err(outbe_tee::TransportError::InProcess(_))
    ));
    let (_, response) = crate::host::canary_domain_request(&request).unwrap();
    assert!(
        matches!(response, EnclaveResponse::TributeOfferBatch { results, .. } if results.is_empty())
    );
    assert_eq!(context.network_encryption_public().unwrap(), before);
}

#[test]
fn account_export_dispatch_binds_owner_ledger_and_ephemeral_recipient() {
    use crate::protocol::{derive_account_keys_message, eip191_hash, Ledger};
    let context = context(7);
    context
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let _runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some((
        context,
        dispatch_payload,
    )));
    let owner_key = k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
    let account = alloy_primitives::Address::from_public_key(owner_key.verifying_key());
    let requester =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([8; 32])).to_bytes();
    let hash = eip191_hash(&derive_account_keys_message(
        Ledger::Gratis,
        account,
        B256::from(requester),
    ));
    let (signature, recovery) = owner_key.sign_prehash_recoverable(hash.as_slice()).unwrap();
    let mut owner_sig = signature.to_bytes().to_vec();
    owner_sig.push(recovery.to_byte());
    let request = EnclaveRequest::DeriveAccountKeys {
        ledger: Ledger::Gratis,
        account,
        requester_ephemeral_pubkey: requester,
        owner_sig,
    };
    let response = crate::try_domain_account_query(&request).unwrap().unwrap();
    assert!(
        matches!(response, EnclaveResponse::AccountKeysSealed {account: actual, sealed, ..} if actual == account && sealed.len() == 80)
    );
    let mut wrong_ledger = request.clone();
    if let EnclaveRequest::DeriveAccountKeys { ledger, .. } = &mut wrong_ledger {
        *ledger = Ledger::Promis;
    }
    let mut wrong_recipient = request.clone();
    if let EnclaveRequest::DeriveAccountKeys {
        requester_ephemeral_pubkey,
        ..
    } = &mut wrong_recipient
    {
        requester_ephemeral_pubkey[0] ^= 1;
    }
    for request in [wrong_ledger, wrong_recipient] {
        let error = crate::try_domain_account_query(&request)
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, outbe_tee::TransportError::InProcess(_)));
        assert!(std::error::Error::source(&error).is_some());
        assert!(
            !error.is_node_local(),
            "invalid owner proof remains a request rejection"
        );
    }
}

#[test]
fn blocked_account_query_does_not_serialize_execution() {
    use outbe_tee::protocol::{EnclaveResponse as WireResponse, PayloadLane};
    use std::sync::{mpsc, Barrier, OnceLock};
    static GATES: OnceLock<(Barrier, Barrier)> = OnceLock::new();
    fn blocking_query(
        context: &InProcessTeeContext,
        lane: PayloadLane,
        payload: &[u8],
    ) -> std::result::Result<WireResponse, outbe_tee::TransportError> {
        if lane == PayloadLane::AccountQuery {
            let gates = GATES.get().unwrap();
            gates.0.wait();
            gates.1.wait();
        }
        dispatch_payload(context, lane, payload)
    }
    let gates = GATES.get_or_init(|| (Barrier::new(2), Barrier::new(2)));
    let context = context(7);
    context
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let (completed, completion) = mpsc::channel();
    std::thread::scope(|scope| {
        let query_context = Arc::clone(&context);
        let query = scope.spawn(move || {
            let _runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(
                Some((query_context, blocking_query)),
            );
            crate::try_domain_account_query(&EnclaveRequest::DeriveAccountKeys {
                ledger: crate::protocol::Ledger::Gratis,
                account: alloy_primitives::Address::repeat_byte(2),
                requester_ephemeral_pubkey: [3; 32],
                owner_sig: Vec::new(),
            })
            .unwrap()
        });
        gates.0.wait();
        scope.spawn(|| {
            let _runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(
                Some((context, blocking_query)),
            );
            let response =
                crate::try_with_domain_enclave(&EnclaveRequest::ProcessTributeOfferBatch {
                    offers: Vec::new(),
                })
                .unwrap()
                .1;
            completed.send(response).unwrap();
        });
        let execution = completion.recv_timeout(std::time::Duration::from_secs(5));
        gates.1.wait();
        assert!(execution
            .expect("execution completed while account query was still blocked")
            .is_ok());
        assert!(query.join().unwrap().is_err());
    });
}
