//! Record, replay, a second enclave, and direct RPC for Gratis and NOD mine.

use std::sync::{Arc, Mutex};

use alloy_primitives::{hex, Address, B256, U256};
use alloy_sol_types::SolError;
use outbe_core_common::confidential_tee::{encode_tagged_payload, WitnessResponseTag, NOD};
use outbe_core_common::nod_encryption::NodTermsV1;
use outbe_fidelity::{FidelityCohortOp, FidelityOpSection};
use outbe_gratis::enclave_client::{apply_gratis_op, mine_encrypted_nod};
use outbe_gratis::nod_mine::{
    inputs_hash, result_preimage, EncryptedNodMintRejectedV1, MineEncryptedNodRequestV1, MineRound,
};
use outbe_gratis::precompile::IGratis;
use outbe_gratis::{
    derive_account_keys, gratis_op_canonical_hash, modify_mac, verify_gratis_op_attestation,
    GratisOp, GratisOpRequest, GratisOpResult, GratisOpStatus, ModifyAuth, ModifyOperation,
};
use outbe_nod::nod_encryption::encrypt_nod;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::{
    in_process_tee_context, replace_in_process_tee_context, set_in_process_tee_context_for_test,
    InProcessTestGuard,
};
use outbe_tee::resident::ResidentOfferMaterial;
use outbe_tee::tee_call::{
    verify_enclave_attestation_lazy, verify_signed_rejection, SignedRejectionRef,
};
use outbe_tee::witness::{
    BlockWitnessFault, CandidateRejection, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1,
    WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};
use outbe_tee::InProcessDispatchError;

const GROUP: &[u8] = b"gratis-witness-group-signature-v1!!!!!";

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

fn offer() -> ResidentOfferMaterial {
    ResidentOfferMaterial::for_test(GROUP.to_vec(), chain_id(), 0, 0).expect("offer material")
}

fn prepared(seed: u8) -> Arc<InProcessTeeContext> {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    context.activate_for_test(offer()).expect("activate offer");
    context
}

fn install(seed: u8) -> (outbe_tee::in_process_dispatch::InProcessTestGuard, [u8; 32]) {
    let context = prepared(seed);
    let leader = context.attestation_public();
    let guard = set_in_process_tee_context_for_test(Some(context));
    (guard, leader)
}

fn record<T>(call: impl FnOnce() -> T) -> (T, EnclaveWitnessTapeV1, [u8; 32]) {
    let (_guard, leader) = install(4);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let value = call();
    let tape = EnclaveWitnessTapeV1 {
        prefix: state.lock().expect("witness lock").staged.clone(),
        txs: Vec::new(),
    };
    (value, tape, leader)
}

fn replay<T>(leader: [u8; 32], tape: EnclaveWitnessTapeV1, call: impl FnOnce() -> T) -> T {
    let _guard = set_in_process_tee_context_for_test(None);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        tape,
    )));
    let _scope = WitnessExecutionScope::enter(state);
    call()
}

fn replay_rejects<T, E>(
    leader: [u8; 32],
    tape: EnclaveWitnessTapeV1,
    call: impl FnOnce() -> Result<T, E>,
) {
    let _guard = set_in_process_tee_context_for_test(None);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        tape,
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    assert!(call().is_err());
    let witness = state.lock().expect("witness lock");
    assert_eq!(
        witness.first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
    assert_eq!(witness.prefix_cursor, 0);
}

fn with_other_enclave<T>(
    leader: [u8; 32],
    tape: EnclaveWitnessTapeV1,
    call: impl FnOnce() -> T,
) -> T {
    let context = prepared(9);
    assert_ne!(leader, context.attestation_public());
    let _guard = set_in_process_tee_context_for_test(Some(context));
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader),
        tape,
    )));
    let _scope = WitnessExecutionScope::enter(state);
    call()
}

fn payload_of(tape: &EnclaveWitnessTapeV1) -> u8 {
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &tape.prefix[0]
    else {
        panic!("recorded entry is not a payload");
    };
    assert_ne!(*inputs_canonical_hash, B256::ZERO);
    assert_eq!(attestation_tag.len(), 64);
    payload[0]
}

fn account() -> Address {
    Address::repeat_byte(0x11)
}

fn mint_request() -> GratisOpRequest {
    let ctx = prepared(4);
    let state_key = ctx
        .state_key_handle(outbe_core_common::confidential_tee::GRATIS.state_info(), 0)
        .expect("gratis key");
    let keys = derive_account_keys(&state_key, account()).expect("account keys");
    let amount = U256::from(7u64);
    let operation = ModifyOperation {
        account: account(),
        op: GratisOp::Mint,
        amount,
        op_nonce: 0,
        chain_id: chain_id(),
    };
    GratisOpRequest {
        op: GratisOp::Mint,
        chain_id: chain_id(),
        account: account(),
        amount,
        current_balance: Vec::new(),
        current_pledged: Vec::new(),
        modify_auth: ModifyAuth {
            mac: modify_mac(&keys, &operation),
            op_nonce: 0,
        },
        fidelity: None,
    }
}

fn zero_request(account: Address) -> GratisOpRequest {
    GratisOpRequest {
        op: GratisOp::Mint,
        chain_id: chain_id(),
        account,
        amount: U256::ZERO,
        current_balance: Vec::new(),
        current_pledged: Vec::new(),
        modify_auth: ModifyAuth {
            mac: [0; 32],
            op_nonce: 0,
        },
        fidelity: None,
    }
}

fn rejection_reason(error: PrecompileError) -> String {
    let PrecompileError::RevertBytes(bytes) = error else {
        panic!("expected RevertBytes, got {error:?}");
    };
    IGratis::Rejected::abi_decode(&bytes)
        .expect("Rejected")
        .value
}

#[test]
fn applied_op_records_replays_and_accepts_a_different_attestation_key() {
    let request = mint_request();
    let expected = gratis_op_canonical_hash(&request);
    let (recorded, tape, leader) = record(|| apply_gratis_op(request.clone()).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::GratisOpApplied.value()
    );
    assert!(matches!(recorded.status, GratisOpStatus::Applied));
    assert_eq!(recorded.inputs_canonical_hash, expected);
    assert_eq!(recorded.next_op_nonce, 1);
    let replayed = replay(leader, tape.clone(), || {
        apply_gratis_op(request.clone()).unwrap()
    });
    let verified = with_other_enclave(leader, tape, || apply_gratis_op(request).unwrap());
    assert_eq!(replayed, recorded);
    assert_eq!(verified, recorded);
}

#[test]
fn replay_of_a_different_request_stops_before_the_tape_cursor_moves() {
    let first = zero_request(account());
    let second = zero_request(Address::repeat_byte(0x22));
    assert_ne!(
        gratis_op_canonical_hash(&first),
        gratis_op_canonical_hash(&second)
    );
    let (_recorded, tape, leader) = record(|| apply_gratis_op(first).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::GratisOpApplied.value()
    );
    replay_rejects(leader, tape, || apply_gratis_op(second));
}

#[test]
fn rpc_direct_checks_the_executing_context_and_ignores_a_later_swap() {
    let first = prepared(4);
    let leader = first.attestation_public();
    let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&first)));
    let second = prepared(9);
    assert_ne!(leader, second.attestation_public());
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::RpcDirect,
        Some(second.attestation_public()),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    assert!(outbe_tee::call_context::current_witness_state().is_some());

    let request = mint_request();
    let expected = gratis_op_canonical_hash(&request);
    let recorded = apply_gratis_op(request.clone()).unwrap();
    assert!(matches!(recorded.status, GratisOpStatus::Applied));
    verify_gratis_op_attestation(&leader, expected, &recorded, &recorded.attestation_tag).unwrap();
    assert!(verify_gratis_op_attestation(
        &second.attestation_public(),
        expected,
        &recorded,
        &recorded.attestation_tag,
    )
    .is_err());
    let rejected = apply_gratis_op(zero_request(account())).unwrap();
    assert_eq!(
        match &rejected.status {
            GratisOpStatus::Rejected { reason } => reason.as_str(),
            GratisOpStatus::Applied => panic!("zero amount was applied"),
        },
        "amount and account must be nonzero"
    );

    replace_in_process_tee_context(&first, Arc::clone(&second), || {
        Ok::<(), InProcessDispatchError>(())
    })
    .unwrap();
    assert_eq!(
        in_process_tee_context()
            .expect("successor")
            .attestation_public(),
        second.attestation_public()
    );
    let replaced = apply_gratis_op(request).unwrap();
    verify_gratis_op_attestation(
        &second.attestation_public(),
        expected,
        &replaced,
        &replaced.attestation_tag,
    )
    .unwrap();
    assert!(
        verify_gratis_op_attestation(&leader, expected, &recorded, &recorded.attestation_tag)
            .is_ok()
    );
    let witness = state.lock().expect("witness lock");
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 0);
}

fn mine_request(flip: bool) -> MineEncryptedNodRequestV1 {
    mine_request_amount(123, flip)
}

fn mine_request_amount(amount: u64, flip: bool) -> MineEncryptedNodRequestV1 {
    let ctx = prepared(4);
    let nod_key = ctx.state_key_handle(NOD.state_info(), 0).expect("nod key");
    let gratis_key = ctx
        .state_key_handle(outbe_core_common::confidential_tee::GRATIS.state_info(), 0)
        .expect("gratis key");
    let owner = Address::repeat_byte(3);
    let amount = U256::from(amount);
    let nod = encrypt_nod(
        &nod_key,
        NodTermsV1 {
            chain_id: 1,
            nod_id: U256::from_be_bytes([4; 32]),
            owner,
            emission_day: 20250115,
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 978,
        },
        amount,
        amount,
    )
    .unwrap();
    let keys = derive_account_keys(&gratis_key, owner).unwrap();
    let mut mac = modify_mac(
        &keys,
        &ModifyOperation {
            account: owner,
            op: GratisOp::Mint,
            amount,
            op_nonce: 0,
            chain_id: chain_id(),
        },
    );
    if flip {
        mac[0] ^= 1;
    }
    MineEncryptedNodRequestV1 {
        nod,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth { mac, op_nonce: 0 },
        fidelity: FidelityOpSection {
            op: FidelityCohortOp::In,
            timestamp: 1_000_000,
            first_qualified_start: 0,
            current_blob: Vec::new(),
        },
    }
}

#[test]
fn nod_mine_records_tag_9_and_a_flipped_mac_records_tag_8() {
    let request = mine_request(false);
    let (mined, tape, leader) = record(|| mine_encrypted_nod(request.clone()).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::EncryptedNodMinedV1.value()
    );
    assert_eq!(mined.next_op_nonce, 1);
    let replayed = replay(leader, tape, || mine_encrypted_nod(request).unwrap());
    assert_eq!(replayed, mined);

    let refused = mine_request(true);
    let (error, tape, leader) = record(|| mine_encrypted_nod(refused.clone()).unwrap_err());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::EncryptedNodMintRejectedV1.value()
    );
    let reason = rejection_reason(error);
    assert!(reason.contains("invalid modify authorization"));
    let replayed = replay(leader, tape, || mine_encrypted_nod(refused).unwrap_err());
    assert_eq!(rejection_reason(replayed), reason);
}

fn dormant(seed: u8) -> Arc<InProcessTeeContext> {
    Arc::new(outbe_tee::test_utils::resident_context(seed, binding()))
}

fn classify(
    user_candidate: bool,
    context: Arc<InProcessTeeContext>,
    call: impl FnOnce() -> PrecompileError,
) -> (PrecompileError, WitnessState) {
    let leader = context.attestation_public();
    let _guard = set_in_process_tee_context_for_test(Some(context));
    let mut blank = WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    );
    if user_candidate {
        blank.tx_index = Some(1);
        blank.is_user_candidate = true;
    }
    let state = Arc::new(Mutex::new(blank));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let error = call();
    let witness = state.lock().expect("witness lock").clone();
    (error, witness)
}

fn unavailable_message(error: PrecompileError) -> String {
    match error {
        PrecompileError::EnclaveUnavailable(message) => message,
        other => panic!("expected EnclaveUnavailable, got {other:?}"),
    }
}

fn assert_no_tape(witness: &WitnessState) {
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 0);
}

#[test]
fn unactivated_context_is_local_unavailable_for_gratis_and_nod() {
    for user_candidate in [false, true] {
        let (error, witness) = classify(user_candidate, dormant(4), || {
            apply_gratis_op(mint_request()).unwrap_err()
        });
        let message = unavailable_message(error);
        assert!(
            message.contains("resident offer material unavailable"),
            "{message}"
        );
        assert_no_tape(&witness);
        assert!(witness.candidate_rejection.is_none());
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::LocalUnavailable)
        );

        let (error, witness) = classify(user_candidate, dormant(4), || {
            mine_encrypted_nod(mine_request(false)).unwrap_err()
        });
        let message = unavailable_message(error);
        assert!(
            message.contains("resident offer material unavailable"),
            "{message}"
        );
        assert_no_tape(&witness);
        assert!(witness.candidate_rejection.is_none());
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::LocalUnavailable)
        );
    }
}

#[test]
fn nod_chain_mismatch_is_unsigned_for_a_user_and_tape_invalid_for_prefix() {
    for user_candidate in [false, true] {
        let (error, witness) = classify(user_candidate, prepared(4), || {
            let mut request = mine_request(false);
            request.nod.terms.chain_id = 99;
            mine_encrypted_nod(request).unwrap_err()
        });
        assert_chain_refusal(user_candidate, error, &witness);
    }
}

#[test]
fn foreign_gratis_chain_records_a_signed_tag_1_rejection() {
    for user_candidate in [false, true] {
        let context = prepared(4);
        let leader = context.attestation_public();
        let _guard = set_in_process_tee_context_for_test(Some(context));
        let mut blank = WitnessState::new(
            WitnessExecutionMode::ProposerRecord,
            Some(leader),
            EnclaveWitnessTapeV1::default(),
        );
        if user_candidate {
            blank.tx_index = Some(1);
            blank.is_user_candidate = true;
        }
        let state = Arc::new(Mutex::new(blank));
        let _scope = WitnessExecutionScope::enter(state.clone());
        let mut request = mint_request();
        request.chain_id = B256::repeat_byte(0x5a);
        let result = apply_gratis_op(request).expect("signed rejection");
        let GratisOpStatus::Rejected { reason } = &result.status else {
            panic!("foreign chain was applied");
        };
        assert_eq!(reason, "invalid modify authorization");
        verify_gratis_op_attestation(
            &leader,
            result.inputs_canonical_hash,
            &result,
            &result.attestation_tag,
        )
        .unwrap();
        assert_eq!(
            rejection_reason(outbe_gratis::errors::GratisError::Rejected(reason.clone()).into()),
            "invalid modify authorization"
        );
        let witness = state.lock().expect("witness lock");
        assert_eq!(witness.staged.len(), 1);
        assert_eq!(
            payload_of(&EnclaveWitnessTapeV1 {
                prefix: witness.staged.clone(),
                txs: Vec::new(),
            }),
            WitnessResponseTag::GratisOpApplied.value()
        );
        assert_eq!(witness.prefix_cursor, 0);
        assert!(witness.candidate_rejection.is_none());
        assert!(witness.first_witness_fault.is_none());
    }
}

fn assert_chain_refusal(user_candidate: bool, error: PrecompileError, witness: &WitnessState) {
    let message = unavailable_message(error);
    assert!(
        message.contains("request chain differs from resident chain"),
        "{message}"
    );
    assert_no_tape(witness);
    if user_candidate {
        assert_eq!(
            witness.candidate_rejection,
            Some(CandidateRejection::UnsignedEnclaveError)
        );
        assert!(witness.first_witness_fault.is_none());
    } else {
        assert!(witness.candidate_rejection.is_none());
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::TapeInvalid)
        );
    }
}

#[test]
fn nod_replay_rejects_a_substituted_request_for_tags_9_and_8() {
    let mined = mine_request_amount(123, false);
    let other_mined = mine_request_amount(124, false);
    assert_ne!(
        inputs_hash(&mined).unwrap(),
        inputs_hash(&other_mined).unwrap()
    );
    let (_value, tape, leader) = record(|| mine_encrypted_nod(mined).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::EncryptedNodMinedV1.value()
    );
    replay_rejects(leader, tape, || mine_encrypted_nod(other_mined));

    let refused = mine_request_amount(123, true);
    let other_refused = mine_request_amount(124, true);
    assert_ne!(
        inputs_hash(&refused).unwrap(),
        inputs_hash(&other_refused).unwrap()
    );
    let (_error, tape, leader) = record(|| mine_encrypted_nod(refused).unwrap_err());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::EncryptedNodMintRejectedV1.value()
    );
    replay_rejects(leader, tape, || mine_encrypted_nod(other_refused));
}

#[test]
fn nod_verifier_accepts_a_different_attestation_key_for_tags_9_and_8() {
    let request = mine_request(false);
    let (mined, tape, leader) = record(|| mine_encrypted_nod(request.clone()).unwrap());
    let local = prepared(9).attestation_public();
    assert_ne!(leader, local);
    let verified = with_other_enclave(leader, tape, || mine_encrypted_nod(request).unwrap());
    assert_eq!(verified, mined);

    let request = mine_request(true);
    let (error, tape, leader) = record(|| mine_encrypted_nod(request.clone()).unwrap_err());
    let reason = rejection_reason(error);
    let local = prepared(9).attestation_public();
    assert_ne!(leader, local);
    let verified = with_other_enclave(leader, tape, || mine_encrypted_nod(request).unwrap_err());
    assert_eq!(rejection_reason(verified), reason);
}

struct RpcRound {
    _guard: InProcessTestGuard,
    state: Arc<Mutex<WitnessState>>,
    _scope: WitnessExecutionScope,
    leader: [u8; 32],
    foreign: [u8; 32],
    first: Arc<InProcessTeeContext>,
    second: Arc<InProcessTeeContext>,
}

fn rpc_round() -> RpcRound {
    let first = prepared(4);
    let leader = first.attestation_public();
    let guard = set_in_process_tee_context_for_test(Some(Arc::clone(&first)));
    let second = prepared(9);
    let foreign = second.attestation_public();
    assert_ne!(leader, foreign);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::RpcDirect,
        Some(foreign),
        EnclaveWitnessTapeV1::default(),
    )));
    let scope = WitnessExecutionScope::enter(state.clone());
    RpcRound {
        _guard: guard,
        state,
        _scope: scope,
        leader,
        foreign,
        first,
        second,
    }
}

fn assert_rpc_quiet(round: &RpcRound) {
    let witness = round.state.lock().expect("witness lock");
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 0);
}

fn assert_mined_tag(key: &[u8; 32], result: &outbe_gratis::nod_mine::MineEncryptedNodResultV1) {
    verify_enclave_attestation_lazy(key, &result.attestation_tag, || result_preimage(result))
        .unwrap();
}

fn assert_refused_tag(key: &[u8; 32], expected: B256, refusal: &EncryptedNodMintRejectedV1) {
    verify_signed_rejection(
        key,
        SignedRejectionRef {
            domain: b"outbe/nod/mint-rejected/v1",
            expected_inputs_hash: expected,
            actual_inputs_hash: refusal.inputs_canonical_hash,
            reason: &refusal.reason,
            tag: &refusal.attestation_tag,
        },
    )
    .unwrap();
}

#[test]
fn nod_rpc_direct_checks_tag_9_before_and_after_a_context_swap() {
    let round = rpc_round();
    let request = mine_request(false);
    let mined = mine_encrypted_nod(request.clone()).unwrap();
    assert_mined_tag(&round.leader, &mined);
    assert!(
        verify_enclave_attestation_lazy(&round.foreign, &mined.attestation_tag, || {
            result_preimage(&mined)
        })
        .is_err()
    );

    replace_in_process_tee_context(&round.first, Arc::clone(&round.second), || {
        Ok::<(), InProcessDispatchError>(())
    })
    .unwrap();
    let replaced = mine_encrypted_nod(request).unwrap();
    assert_mined_tag(&round.foreign, &replaced);
    assert_mined_tag(&round.leader, &mined);
    assert!(
        verify_enclave_attestation_lazy(&round.leader, &replaced.attestation_tag, || {
            result_preimage(&replaced)
        })
        .is_err()
    );
    assert_rpc_quiet(&round);
}

#[test]
fn nod_rpc_direct_checks_tag_8_before_and_after_a_context_swap() {
    let round = rpc_round();
    let request = mine_request(true);
    let expected = inputs_hash(&request).unwrap();
    let MineRound::Refused(refused) = outbe_gratis::nod_mine::mine_round(request.clone()).unwrap()
    else {
        panic!("flipped mac was mined");
    };
    assert_refused_tag(&round.leader, expected, &refused);
    assert!(verify_signed_rejection(
        &round.foreign,
        SignedRejectionRef {
            domain: b"outbe/nod/mint-rejected/v1",
            expected_inputs_hash: expected,
            actual_inputs_hash: refused.inputs_canonical_hash,
            reason: &refused.reason,
            tag: &refused.attestation_tag,
        },
    )
    .is_err());

    replace_in_process_tee_context(&round.first, Arc::clone(&round.second), || {
        Ok::<(), InProcessDispatchError>(())
    })
    .unwrap();
    let MineRound::Refused(replaced) = outbe_gratis::nod_mine::mine_round(request).unwrap() else {
        panic!("flipped mac was mined after the swap");
    };
    assert_refused_tag(&round.foreign, expected, &replaced);
    assert_refused_tag(&round.leader, expected, &refused);
    assert!(verify_signed_rejection(
        &round.leader,
        SignedRejectionRef {
            domain: b"outbe/nod/mint-rejected/v1",
            expected_inputs_hash: expected,
            actual_inputs_hash: replaced.inputs_canonical_hash,
            reason: &replaced.reason,
            tag: &replaced.attestation_tag,
        },
    )
    .is_err());
    assert_rpc_quiet(&round);
}

const FROZEN_MINE_REQUEST: &str = r#"{"nod":{"terms":{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978},"encryption_binding":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_gratis_amount":[65],"encrypted_nominal_amount":[66]},"current_balance":[81,82],"modify_auth":{"mac":[51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51],"op_nonce":5},"fidelity":{"op":"Out","timestamp":123,"first_qualified_start":124,"current_blob":[65,66]}}"#;

fn frozen_bytes<const N: usize>(text: &str) -> [u8; N] {
    hex::decode(text)
        .expect("hex")
        .try_into()
        .unwrap_or_else(|_| panic!("expected {N} bytes"))
}

fn frozen_refusal_tape(reason: &str) -> (EnclaveWitnessTapeV1, [u8; 32]) {
    let inputs = B256::from(frozen_bytes::<32>(
        "e66620bdb18c3bed4b4f00a9d2c64efcb870fa3cff2d6cf6b3043541bf91f855",
    ));
    let leader =
        frozen_bytes::<32>("f80cccdce4ae1c07ae208a2adf99a310ae4207e0306fa0236110b06827bbb8d0");
    let tag = frozen_bytes::<64>(
        "f4c88413f0d2536bf74b5b8d40e6af31475e5e5b4dab2dd4c7d6dd802c2d7ac607a353a351a82f6c2d442db10bee01ac8df1bf957c9e35900492cad99dd0a502",
    );
    let body = EncryptedNodMintRejectedV1 {
        reason: reason.to_owned(),
        inputs_canonical_hash: inputs,
        attestation_tag: Vec::new(),
    };
    let payload = encode_tagged_payload(WitnessResponseTag::EncryptedNodMintRejectedV1, &body)
        .expect("tag 8 payload");
    let tape = EnclaveWitnessTapeV1 {
        prefix: vec![EnclaveWitnessEntryV1::Payload {
            payload,
            inputs_canonical_hash: inputs,
            attestation_tag: tag.to_vec(),
        }],
        txs: Vec::new(),
    };
    (tape, leader)
}

#[test]
fn frozen_nod_refusal_replays_through_the_host_client() {
    let request: MineEncryptedNodRequestV1 = serde_json::from_str(FROZEN_MINE_REQUEST).unwrap();
    let expected = inputs_hash(&request).unwrap();
    assert_eq!(
        expected,
        B256::from(frozen_bytes::<32>(
            "e66620bdb18c3bed4b4f00a9d2c64efcb870fa3cff2d6cf6b3043541bf91f855"
        ))
    );
    let (tape, leader) = frozen_refusal_tape("bad \"quote\"\nline");
    let _guard = set_in_process_tee_context_for_test(None);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        tape,
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let error = mine_encrypted_nod(request).unwrap_err();
    assert_eq!(rejection_reason(error), "bad \"quote\"\nline");
    let witness = state.lock().expect("witness lock");
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 1);
    assert!(witness.first_witness_fault.is_none());
    assert!(witness.candidate_rejection.is_none());
}

#[test]
fn changed_frozen_nod_reason_is_tape_invalid() {
    let request: MineEncryptedNodRequestV1 = serde_json::from_str(FROZEN_MINE_REQUEST).unwrap();
    let (tape, leader) = frozen_refusal_tape("changed");
    replay_rejects(leader, tape, || mine_encrypted_nod(request));
}

#[test]
fn promis_tag_on_the_gratis_tape_is_rejected_by_the_typed_codec() {
    let request = mint_request();
    let real_hash = gratis_op_canonical_hash(&request);
    assert_ne!(real_hash, B256::ZERO);
    let mut foreign = encode_tagged_payload(
        WitnessResponseTag::GratisOpApplied,
        &GratisOpResult {
            status: GratisOpStatus::Applied,
            new_balance: Vec::new(),
            new_pledged: Vec::new(),
            event_amount: U256::ZERO,
            next_op_nonce: 0,
            fidelity: None,
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: Vec::new(),
            encrypted_event_amount: Vec::new(),
        },
    )
    .expect("canonical gratis body");
    assert_eq!(foreign[0], WitnessResponseTag::GratisOpApplied.value());
    foreign[0] = WitnessResponseTag::PromisOpApplied.value();
    let cases = [
        (vec![WitnessResponseTag::PromisOpApplied.value()], real_hash),
        (foreign, B256::ZERO),
    ];
    for (payload, inputs_canonical_hash) in cases {
        let tape = EnclaveWitnessTapeV1 {
            prefix: vec![EnclaveWitnessEntryV1::Payload {
                payload,
                inputs_canonical_hash,
                attestation_tag: vec![0; 64],
            }],
            txs: Vec::new(),
        };
        let _guard = set_in_process_tee_context_for_test(None);
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierReplay,
            Some([0x44; 32]),
            tape,
        )));
        let _scope = WitnessExecutionScope::enter(state.clone());
        let error = apply_gratis_op(request.clone()).unwrap_err();
        let sentence = format!("{error}");
        assert!(
            sentence.contains("witness payload tag 2 does not match expected 1"),
            "{sentence}"
        );
        assert!(
            !sentence.contains("tee_enclave_nondeterminism"),
            "{sentence}"
        );
        let witness = state.lock().expect("witness lock");
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::TapeInvalid)
        );
        assert_eq!(witness.prefix_cursor, 0);
    }
}
