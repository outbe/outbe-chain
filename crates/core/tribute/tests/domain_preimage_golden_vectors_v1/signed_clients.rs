//! Frozen Ed25519 answers accepted by the public witness clients.
use super::enclave_response::EnclaveResponse;
use super::fixtures::*;
use alloy_primitives::{hex, B256, U256};
use alloy_sol_types::SolError;
use outbe_core_common::confidential_tee::{attestation, WitnessResponseTag};
use outbe_core_common::nod_encryption::EncryptedNodV1;
use outbe_gratis::enclave_client::mine_encrypted_nod;
use outbe_gratis::nod_mine::MineEncryptedNodRequestV1;
use outbe_gratis::precompile::IGratis;
use outbe_nod::nod_host::encrypt_nod_quote;
use outbe_primitives::error::PrecompileError;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::witness::{
    EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, TxWitnessV1, WitnessExecutionMode,
    WitnessExecutionScope, WitnessState,
};
use outbe_tribute::confidential::host::{
    encrypt_nod_for_tribute, issue_nod_from_tribute, NodEncryptedForTributeV1,
    NodIssuedFromTributeV1,
};
use outbe_tribute::confidential::{IssueNodFromTributeOutcomeV1, IssueNodFromTributeRequestV1};
use std::sync::{Arc, Mutex};

const LEADER: &str = "f80cccdce4ae1c07ae208a2adf99a310ae4207e0306fa0236110b06827bbb8d0";

fn leader() -> [u8; 32] {
    hex::decode(LEADER).unwrap().try_into().unwrap()
}

fn wire(tag: WitnessResponseTag, body: &impl serde::Serialize) -> Vec<u8> {
    let mut payload = vec![tag.value()];
    payload.extend(postcard::to_allocvec(body).unwrap());
    payload
}

fn replay(payload: Vec<u8>, inputs: B256, tag: Vec<u8>, call: impl FnOnce()) {
    let _absent = set_in_process_tee_context_for_test(None);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader()),
        EnclaveWitnessTapeV1 {
            prefix: vec![EnclaveWitnessEntryV1::Payload {
                payload,
                inputs_canonical_hash: inputs,
                attestation_tag: tag,
            }],
            txs: Vec::new(),
        },
    )));
    let scope = WitnessExecutionScope::enter(Arc::clone(&state));
    call();
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 1);
    assert_eq!(witness.tx_cursor, 0);
    assert_eq!(witness.replay_cursor, 0);
    assert_eq!(witness.first_witness_fault, None);
    drop(witness);
    drop(scope);
}

fn replay_tx(payload: Vec<u8>, inputs: B256, tag: Vec<u8>, call: impl FnOnce()) {
    let _absent = set_in_process_tee_context_for_test(None);
    let mut witness = WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader()),
        EnclaveWitnessTapeV1 {
            prefix: Vec::new(),
            txs: vec![TxWitnessV1 {
                tx_index: 0,
                entries: vec![EnclaveWitnessEntryV1::Payload {
                    payload,
                    inputs_canonical_hash: inputs,
                    attestation_tag: tag,
                }],
            }],
        },
    );
    witness.tx_index = Some(0);
    let state = Arc::new(Mutex::new(witness));
    let scope = WitnessExecutionScope::enter(Arc::clone(&state));
    call();
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(witness.tx_cursor, 0);
    assert_eq!(witness.replay_cursor, 1);
    assert_eq!(witness.first_witness_fault, None);
    drop(witness);
    drop(scope);
}

#[test]
fn nod_creation_issue_and_quote_signed_vectors() {
    {
        let response: EnclaveResponse = parse(
            r#"{"NodEncryptedForTributeV1":{"nod":{"terms":{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"league_id":7,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978},"encryption_binding":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_gratis_amount":[65]},"inputs_canonical_hash":"0xeac818d561be445d8568ddf26c61694de8fe1bffbbb5ec75be7e0ac762cec63d","attestation_tag":[63,40,73,183,255,101,48,28,37,227,200,230,14,112,73,196,45,1,236,123,107,178,78,76,70,31,140,240,139,166,89,141,84,92,112,99,178,208,203,196,177,47,155,220,224,27,158,38,123,66,123,147,57,81,170,221,8,224,114,172,29,253,167,11]}}"#,
        );
        let EnclaveResponse::NodEncryptedForTributeV1 {
            nod,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("create vector");
        };
        let payload = wire(
            WitnessResponseTag::NodEncryptedForTributeV1,
            &NodEncryptedForTributeV1 {
                nod: nod.clone(),
                inputs_canonical_hash,
                attestation_tag: Vec::new(),
            },
        );
        let tribute = parse(TRIBUTE);
        let terms = parse(TERMS);
        replay(payload, inputs_canonical_hash, attestation_tag, || {
            assert_eq!(
                encrypt_nod_for_tribute(tribute, terms, U256::from(17)).unwrap(),
                nod
            );
        });
    }
    {
        let response: EnclaveResponse = parse(
            r#"{"NodIssuedFromTributeV1":{"outcome":{"Issued":{"terms":{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"league_id":7,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978},"encryption_binding":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_gratis_amount":[65]}},"inputs_canonical_hash":"0xe976b2eb50785de6c40f261fba8bc3274b28120f228f383b8ed61e73398fd035","attestation_tag":[1,175,146,254,230,254,237,99,228,100,216,33,237,141,148,106,150,5,203,187,14,97,68,226,124,6,108,112,229,156,227,209,11,175,175,209,148,39,168,138,96,222,241,54,40,94,7,30,199,239,30,48,129,212,239,170,251,3,33,175,225,249,123,8]}}"#,
        );
        let EnclaveResponse::NodIssuedFromTributeV1 {
            outcome,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("issue vector");
        };
        let payload = wire(
            WitnessResponseTag::NodIssuedFromTributeV1,
            &NodIssuedFromTributeV1 {
                outcome: outcome.clone(),
                inputs_canonical_hash,
                attestation_tag: Vec::new(),
            },
        );
        let request: IssueNodFromTributeRequestV1 = parse(ISSUE_REQUEST);
        replay(payload, inputs_canonical_hash, attestation_tag, || {
            assert_eq!(
                issue_nod_from_tribute(request).unwrap(),
                IssueNodFromTributeOutcomeV1::Issued(parse(NOD))
            );
        });
        assert_eq!(outcome, IssueNodFromTributeOutcomeV1::Issued(parse(NOD)));
    }
    {
        let response: EnclaveResponse = parse(
            r#"{"NodQuoteEncryptedV1":{"encrypted_payment":[65,66],"inputs_canonical_hash":"0xb665b0b89642a4117ce7ae1b6565d2d33e1d3462046879c722c0b05188ca4263","attestation_tag":[180,156,68,221,19,140,22,240,154,209,159,194,93,52,89,138,178,249,188,154,182,56,248,183,20,128,141,183,64,57,135,176,2,33,231,38,45,36,16,190,242,217,174,198,153,155,218,116,113,54,16,88,30,138,160,183,34,179,116,83,32,81,140,11]}}"#,
        );
        let EnclaveResponse::NodQuoteEncryptedV1 {
            encrypted_payment,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("quote vector");
        };
        let body = outbe_nod::nod_host::NodQuoteEncryptedV1 {
            encrypted_payment: encrypted_payment.clone(),
            inputs_canonical_hash,
            attestation_tag: Vec::new(),
        };
        let nod: EncryptedNodV1 = parse(NOD);
        let context: outbe_core_common::nod_encryption::NodQuoteContextV1 = parse(QUOTE_CONTEXT);
        let payload = wire(WitnessResponseTag::NodQuoteEncryptedV1, &body);
        replay_tx(payload, inputs_canonical_hash, attestation_tag, || {
            assert_eq!(
                encrypt_nod_quote(&nod, context, U256::from(18)).unwrap(),
                vec![65, 66]
            );
        });
    }
}

#[test]
fn signed_mint_rejection_vector() {
    let response: EnclaveResponse = parse(
        r#"{"EncryptedNodMintRejectedV1":{"reason":"bad \"quote\"\nline","inputs_canonical_hash":"0x5b2a54224b14481d7dd879f3b7a37547dcfae32f9190da9bd114d0b6f7ba82f2","attestation_tag":[236,236,212,239,73,24,175,50,219,36,121,12,177,176,214,6,214,203,233,245,221,141,234,38,250,92,20,249,26,41,6,162,194,126,232,77,213,59,134,231,67,191,181,227,60,24,226,108,204,152,83,7,136,67,137,253,144,205,35,111,164,124,105,7]}}"#,
    );
    let EnclaveResponse::EncryptedNodMintRejectedV1 {
        reason,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        panic!("mint rejection");
    };
    let body = outbe_gratis::nod_mine::EncryptedNodMintRejectedV1 {
        reason: reason.clone(),
        inputs_canonical_hash,
        attestation_tag: Vec::new(),
    };
    let payload = wire(WitnessResponseTag::EncryptedNodMintRejectedV1, &body);
    let request: MineEncryptedNodRequestV1 = parse(MINE_REQUEST);
    replay(payload, inputs_canonical_hash, attestation_tag, || {
        let error = mine_encrypted_nod(request).unwrap_err();
        let PrecompileError::RevertBytes(bytes) = &error else {
            panic!("mint refusal is revert bytes, got {error:?}");
        };
        let decoded = IGratis::Rejected::abi_decode(bytes).expect("IGratis::Rejected");
        assert_eq!(decoded.value, reason);
        assert_eq!(
            bytes.as_ref(),
            IGratis::Rejected {
                value: reason.clone(),
            }
            .abi_encode()
        );
        assert_eq!(
            bytes.as_ref(),
            hex::decode(concat!(
                "60fcb063",
                "0000000000000000000000000000000000000000000000000000000000000020",
                "0000000000000000000000000000000000000000000000000000000000000010",
                "626164202271756f7465220a6c696e65",
                "00000000000000000000000000000000",
            ))
            .unwrap()
        );
    });
}

#[test]
fn signed_rejection_preimage_and_verifier_vectors() {
    use outbe_tee::tee_call::{verify_signed_rejection, SignedRejectionRef};
    let inputs = B256::repeat_byte(0x11);
    let reason = "bad \"quote\"\nline";
    for (domain, expected, signature) in [
        (b"outbe/nod/mint-rejected/v1".as_slice(), "6f757462652f6e6f642f6d696e742d72656a65637465642f7631111111111111111111111111111111111111111111111111111111111111111122626164205c2271756f74655c225c6e6c696e6522", "3dd25c93a3b7d62ebb5cae88db35be6860281e07e8338dc2e16ca948025e28a36b5aa597a2ac6e555c60864b05a817acb95c638202c257eeb52dbfadc49ee40e"),
        (b"outbe/lysis/rejected".as_slice(), "6f757462652f6c797369732f72656a6563746564111111111111111111111111111111111111111111111111111111111111111122626164205c2271756f74655c225c6e6c696e6522", "f0b204ce93aaf81455ff89164f15ff957a9cfaabda823fb53ea111c20a0863191b14ccbb12b8d5ef88ac016ba365f906ca5a971dda74b9278ab8743cf9502f0d"),
        (b"outbe/fidelity/query-rejected/v1".as_slice(), "6f757462652f666964656c6974792f71756572792d72656a65637465642f7631111111111111111111111111111111111111111111111111111111111111111122626164205c2271756f74655c225c6e6c696e6522", "f2f6cbd5817c2c233ec02c10ca7b15ca4bc03f96384825365661757b72a5dbf642988500a301612615975da9969407fda8e20ac0622433418203e889628b8e0d"),
    ] {
        let preimage = attestation(domain, inputs, reason).unwrap();
        assert_eq!(preimage, hex::decode(expected).unwrap());
        let tag = hex::decode(signature).unwrap();
        let claim = SignedRejectionRef { domain, expected_inputs_hash: inputs, actual_inputs_hash: inputs, reason, tag: &tag };
        verify_signed_rejection(&hex!("f80cccdce4ae1c07ae208a2adf99a310ae4207e0306fa0236110b06827bbb8d0"), claim).unwrap();
        assert!(verify_signed_rejection(&hex!("f80cccdce4ae1c07ae208a2adf99a310ae4207e0306fa0236110b06827bbb8d0"), SignedRejectionRef { reason: "changed", ..claim }).is_err());
    }
}
