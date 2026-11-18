//! Tag 22 witness oracles for `mine_gratis_from_promis` on proposer tx 0.
//!
//! The three checks match the tag 2 oracles: an independent payload entry,
//! a signed semantic divergence, and non-canonical tag-22 bytes.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    decode_tagged_payload, encode_tagged_payload, encrypt_tee_amount, BalanceTransition,
    WitnessResponseTag,
};
use outbe_fidelity::{FidelityCohortOp, FidelityOpSection};
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_promis::enclave_client::mine_gratis_from_promis;
use outbe_promis::{
    derive_account_keys, derive_view_key, mine_gratis_from_promis_attestation_preimage, modify_mac,
    verify_mine_gratis_from_promis_attestation, MineGratisFromPromisRequestV1,
    MineGratisFromPromisResultV1, MineGratisFromPromisStatusV1, ModifyAuth, ModifyOperation,
    PromisOp,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::protocol::{EnclaveResponse, PayloadLane};
use outbe_tee::resident::ResidentOfferMaterial;
use outbe_tee::witness::{
    BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, TxWitnessV1,
    WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};
use outbe_tee::TransportError;

const GROUP: &[u8] = b"promis-witness-group-signature-v1!!!!!";

fn binding() -> NetworkBindingV1 {
    NetworkBindingV1 {
        chain_id: U256::from(1u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
    }
}

fn chain_id() -> B256 {
    B256::from(binding().chain_id)
}

fn prepared(seed: u8) -> Arc<InProcessTeeContext> {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    let offer =
        ResidentOfferMaterial::for_test(GROUP.to_vec(), chain_id(), 0, 0).expect("offer material");
    context.activate_for_test(offer).expect("activate offer");
    context
}

fn reject_payload(
    _: &InProcessTeeContext,
    _: PayloadLane,
    _: &[u8],
) -> std::result::Result<EnclaveResponse, TransportError> {
    Err(TransportError::UnexpectedResponse)
}

fn tx_witness(
    mode: WitnessExecutionMode,
    leader: [u8; 32],
    tape: EnclaveWitnessTapeV1,
) -> WitnessState {
    let mut witness = WitnessState::new(mode, Some(leader), tape);
    witness.tx_index = Some(0);
    witness
}

fn tx_tape(entry: EnclaveWitnessEntryV1) -> EnclaveWitnessTapeV1 {
    EnclaveWitnessTapeV1 {
        prefix: Vec::new(),
        txs: vec![TxWitnessV1 {
            tx_index: 0,
            entries: vec![entry],
        }],
    }
}

fn mine_request(amount: u64) -> MineGratisFromPromisRequestV1 {
    let ctx = prepared(4);
    let account = Address::repeat_byte(0x11);
    let amount = U256::from(amount);
    let chain = chain_id();
    let promis = ctx
        .state_key_handle(outbe_core_common::confidential_tee::PROMIS.state_info(), 0)
        .expect("promis key");
    let gratis = ctx
        .state_key_handle(outbe_core_common::confidential_tee::GRATIS.state_info(), 0)
        .expect("gratis key");
    let balance = outbe_promis::cipher::write_amount(
        &derive_view_key(&promis, account).unwrap(),
        BalanceTransition {
            account,
            field: 0,
            previous_version: 0,
            amount,
            input_hash: B256::ZERO,
        },
    )
    .unwrap();
    let encrypted = encrypt_tee_amount(
        &ctx.network_encryption_public().unwrap(),
        chain,
        account,
        amount,
    )
    .unwrap();
    let promis_keys = derive_account_keys(&promis, account).unwrap();
    let gratis_keys = outbe_gratis::derive_account_keys(&gratis, account).unwrap();
    MineGratisFromPromisRequestV1 {
        chain_id: chain,
        account,
        encrypted_promis_amount: encrypted.to_vec(),
        current_promis_balance: balance,
        promis_modify_auth: ModifyAuth {
            mac: modify_mac(
                &promis_keys,
                &ModifyOperation {
                    account,
                    op: PromisOp::Burn,
                    amount,
                    op_nonce: 0,
                    chain_id: chain,
                },
            ),
            op_nonce: 0,
        },
        current_gratis_balance: Vec::new(),
        gratis_modify_auth: ModifyAuth {
            mac: outbe_gratis::modify_mac(
                &gratis_keys,
                &outbe_gratis::ModifyOperation {
                    account,
                    op: outbe_gratis::GratisOp::Mint,
                    amount,
                    op_nonce: 0,
                    chain_id: chain,
                },
            ),
            op_nonce: 0,
        },
        fidelity: FidelityOpSection {
            op: FidelityCohortOp::In,
            timestamp: 1_700_000_000,
            first_qualified_start: 0,
            current_blob: Vec::new(),
        },
    }
}

fn tag22_entry(result: &MineGratisFromPromisResultV1) -> EnclaveWitnessEntryV1 {
    let mut cleared = result.clone();
    cleared.attestation_tag.clear();
    let payload = encode_tagged_payload(WitnessResponseTag::GratisMinedFromPromisV1, &cleared)
        .expect("tag 22 body");
    EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash: result.inputs_canonical_hash,
        attestation_tag: result.attestation_tag.clone(),
    }
}

fn resign(
    context: &InProcessTeeContext,
    mut result: MineGratisFromPromisResultV1,
) -> MineGratisFromPromisResultV1 {
    result.attestation_tag.clear();
    let preimage =
        mine_gratis_from_promis_attestation_preimage(result.inputs_canonical_hash, &result)
            .expect("preimage");
    result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
    result
}

fn execute(
    context: Arc<InProcessTeeContext>,
    request: MineGratisFromPromisRequestV1,
) -> MineGratisFromPromisResultV1 {
    let _guard = set_in_process_tee_context_for_test(Some((context, reject_payload)));
    mine_gratis_from_promis(request).expect("mine")
}

fn assert_applied(result: &MineGratisFromPromisResultV1) {
    assert!(matches!(
        result.status,
        MineGratisFromPromisStatusV1::Applied
    ));
}

#[test]
fn recorded_tag_22_entry_matches_an_independent_encode() {
    let request = mine_request(9);
    let context = prepared(4);
    let leader = context.attestation_public();
    let _guard = set_in_process_tee_context_for_test(Some((Arc::clone(&context), reject_payload)));
    let state = Arc::new(Mutex::new(tx_witness(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    )));
    let recorded = {
        let _scope = WitnessExecutionScope::enter(state.clone());
        mine_gratis_from_promis(request.clone()).unwrap()
    };
    assert_applied(&recorded);
    let witness = state.lock().expect("witness lock");
    assert!(witness.first_witness_fault.is_none());
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(witness.replay_cursor, 0);
    assert_eq!(witness.staged.len(), 1);
    let entry = witness.staged[0].clone();
    drop(witness);
    let direct = mine_gratis_from_promis(request).unwrap();
    assert_applied(&direct);
    assert_eq!(direct, recorded);
    let reference = tag22_entry(&direct);
    assert_eq!(entry, reference);
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &entry
    else {
        panic!("recorded entry is not a payload");
    };
    assert_eq!(
        payload[0],
        WitnessResponseTag::GratisMinedFromPromisV1.value()
    );
    assert_eq!(*inputs_canonical_hash, direct.inputs_canonical_hash);
    assert_eq!(attestation_tag, &direct.attestation_tag);
    let mut cleared = direct.clone();
    cleared.attestation_tag.clear();
    let decoded: MineGratisFromPromisResultV1 =
        decode_tagged_payload(WitnessResponseTag::GratisMinedFromPromisV1, payload).unwrap();
    assert_eq!(decoded, cleared);
}

#[test]
fn semantic_divergence_on_tag_22_latches_semantic_mismatch() {
    let request = mine_request(11);
    let leader = prepared(4);
    let honest = execute(Arc::clone(&leader), request.clone());
    assert_applied(&honest);
    let divergent = resign(
        &leader,
        MineGratisFromPromisResultV1 {
            new_promis_balance: vec![0xaa],
            ..honest
        },
    );
    let verifier = prepared(9);
    let local = execute(Arc::clone(&verifier), request.clone());
    assert_applied(&divergent);
    assert_applied(&local);
    assert_eq!(divergent.new_promis_balance, vec![0xaa]);
    assert_ne!(local.new_promis_balance, divergent.new_promis_balance);
    assert_eq!(local.inputs_canonical_hash, divergent.inputs_canonical_hash);
    verify_mine_gratis_from_promis_attestation(
        &leader.attestation_public(),
        divergent.inputs_canonical_hash,
        &divergent,
        &divergent.attestation_tag,
    )
    .unwrap();
    verify_mine_gratis_from_promis_attestation(
        &verifier.attestation_public(),
        local.inputs_canonical_hash,
        &local,
        &local.attestation_tag,
    )
    .unwrap();
    let state = Arc::new(Mutex::new(tx_witness(
        WitnessExecutionMode::VerifierWithEnclave,
        leader.attestation_public(),
        tx_tape(tag22_entry(&divergent)),
    )));
    let error = {
        let _guard = set_in_process_tee_context_for_test(Some((verifier, reject_payload)));
        let _scope = WitnessExecutionScope::enter(state.clone());
        mine_gratis_from_promis(request).unwrap_err()
    };
    assert!(error.to_string().contains("SemanticMismatch"));
    let witness = state.lock().expect("witness lock");
    assert_eq!(
        witness.first_witness_fault,
        Some(BlockWitnessFault::SemanticMismatch)
    );
    assert_eq!(witness.replay_cursor, 0);
    assert_eq!(witness.prefix_cursor, 0);
}

#[test]
fn non_canonical_tag_22_bytes_latch_tape_invalid_at_cursor_zero() {
    let request = mine_request(13);
    let leader_ctx = prepared(4);
    let leader = leader_ctx.attestation_public();
    let honest = execute(leader_ctx, request.clone());
    assert_applied(&honest);
    let mut clean = honest.clone();
    clean.attestation_tag.clear();
    let mut trailing =
        encode_tagged_payload(WitnessResponseTag::GratisMinedFromPromisV1, &clean).unwrap();
    trailing.push(0x00);
    let unstripped = encode_tagged_payload(
        WitnessResponseTag::GratisMinedFromPromisV1,
        &MineGratisFromPromisResultV1 {
            attestation_tag: honest.attestation_tag.clone(),
            ..clean
        },
    )
    .unwrap();
    for payload in [trailing, unstripped] {
        let entry = EnclaveWitnessEntryV1::Payload {
            payload,
            inputs_canonical_hash: honest.inputs_canonical_hash,
            attestation_tag: honest.attestation_tag.clone(),
        };
        for mode in [
            WitnessExecutionMode::VerifierReplay,
            WitnessExecutionMode::VerifierWithEnclave,
        ] {
            let state = Arc::new(Mutex::new(tx_witness(mode, leader, tx_tape(entry.clone()))));
            let _guard = if mode == WitnessExecutionMode::VerifierWithEnclave {
                set_in_process_tee_context_for_test(Some((prepared(9), reject_payload)))
            } else {
                set_in_process_tee_context_for_test(None)
            };
            let _scope = WitnessExecutionScope::enter(state.clone());
            assert!(mine_gratis_from_promis(request.clone()).is_err());
            let witness = state.lock().expect("witness lock");
            assert_eq!(
                witness.first_witness_fault,
                Some(BlockWitnessFault::TapeInvalid)
            );
            assert_eq!(witness.replay_cursor, 0);
            assert_eq!(witness.prefix_cursor, 0);
        }
    }
}
