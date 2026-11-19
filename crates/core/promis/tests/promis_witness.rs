//! Record, replay, a second enclave, and direct RPC for Promis tags 2 and 22.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    decode_tagged_payload, encode_tagged_payload, encrypt_tee_amount, BalanceTransition,
    WitnessResponseTag,
};
use outbe_fidelity::{FidelityCohortOp, FidelityOpSection};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_promis::enclave_client::{apply_promis_op, mine_gratis_from_promis};
use outbe_promis::{
    derive_account_keys, derive_view_key, mine_gratis_from_promis_canonical_hash, modify_mac,
    promis_op_attestation_preimage, promis_op_canonical_hash,
    verify_mine_gratis_from_promis_attestation, verify_promis_op_attestation,
    MineGratisFromPromisRequestV1, MineGratisFromPromisStatusV1, ModifyAuth, ModifyOperation,
    PromisOp, PromisOpRequest, PromisOpResult, PromisOpStatus,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::{
    in_process_tee_context, replace_in_process_tee_context, set_in_process_tee_context_for_test,
    InProcessTestGuard,
};
use outbe_tee::resident::ResidentOfferMaterial;
use outbe_tee::witness::{
    BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, TxWitnessV1,
    WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};
use outbe_tee::InProcessDispatchError;

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

fn offer() -> ResidentOfferMaterial {
    ResidentOfferMaterial::for_test(GROUP.to_vec(), chain_id(), 0, 0).expect("offer material")
}

fn prepared(seed: u8) -> Arc<InProcessTeeContext> {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    context.activate_for_test(offer()).expect("activate offer");
    context
}

fn dormant(seed: u8) -> Arc<InProcessTeeContext> {
    Arc::new(outbe_tee::test_utils::resident_context(seed, binding()))
}

fn install(seed: u8) -> (InProcessTestGuard, [u8; 32]) {
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

fn tx_witness(
    mode: WitnessExecutionMode,
    leader: [u8; 32],
    tape: EnclaveWitnessTapeV1,
) -> WitnessState {
    let mut witness = WitnessState::new(mode, Some(leader), tape);
    witness.tx_index = Some(0);
    witness
}

fn record_tx<T>(call: impl FnOnce() -> T) -> (T, EnclaveWitnessTapeV1, [u8; 32]) {
    let (_guard, leader) = install(4);
    let state = Arc::new(Mutex::new(tx_witness(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let value = call();
    let witness = state.lock().expect("witness lock");
    assert!(witness.first_witness_fault.is_none());
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(witness.replay_cursor, 0);
    let tape = EnclaveWitnessTapeV1 {
        prefix: Vec::new(),
        txs: vec![TxWitnessV1 {
            tx_index: 0,
            entries: witness.staged.clone(),
        }],
    };
    (value, tape, leader)
}

fn consume_tx<T>(
    mode: WitnessExecutionMode,
    leader: [u8; 32],
    tape: EnclaveWitnessTapeV1,
    call: impl FnOnce() -> T,
) -> T {
    let state = Arc::new(Mutex::new(tx_witness(mode, leader, tape)));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let value = call();
    let witness = state.lock().expect("witness lock");
    assert!(witness.first_witness_fault.is_none());
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(witness.tx_cursor, 0);
    assert_eq!(witness.replay_cursor, 1);
    value
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

fn tx_payload_of(tape: &EnclaveWitnessTapeV1) -> u8 {
    assert!(tape.prefix.is_empty());
    let tx = tape.txs.first().expect("tx lane");
    assert_eq!(tx.tx_index, 0);
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &tx.entries[0]
    else {
        panic!("tx entry is not a payload");
    };
    assert_ne!(*inputs_canonical_hash, B256::ZERO);
    assert_eq!(attestation_tag.len(), 64);
    payload[0]
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

fn promis_key(ctx: &InProcessTeeContext) -> outbe_tee::resident::StateKeyHandle {
    ctx.state_key_handle(outbe_core_common::confidential_tee::PROMIS.state_info(), 0)
        .expect("promis key")
}

fn mint_request(amount: u64) -> PromisOpRequest {
    let ctx = prepared(4);
    let state_key = promis_key(&ctx);
    let keys = derive_account_keys(&state_key, account()).expect("account keys");
    let amount = U256::from(amount);
    let operation = ModifyOperation {
        account: account(),
        op: PromisOp::Mint,
        amount,
        op_nonce: 0,
        chain_id: chain_id(),
    };
    PromisOpRequest {
        op: PromisOp::Mint,
        chain_id: chain_id(),
        account: account(),
        amount,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth {
            mac: modify_mac(&keys, &operation),
            op_nonce: 0,
        },
    }
}

fn zero_request() -> PromisOpRequest {
    PromisOpRequest {
        op: PromisOp::Mint,
        chain_id: chain_id(),
        account: account(),
        amount: U256::ZERO,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth {
            mac: [0; 32],
            op_nonce: 0,
        },
    }
}

fn mine_request(
    amount: u64,
    foreign_chain: bool,
    flip_gratis: bool,
) -> MineGratisFromPromisRequestV1 {
    let ctx = prepared(4);
    let account = account();
    let amount = U256::from(amount);
    let chain = if foreign_chain {
        B256::repeat_byte(0x5a)
    } else {
        chain_id()
    };
    let promis = promis_key(&ctx);
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
    let mut gratis_mac = outbe_gratis::modify_mac(
        &gratis_keys,
        &outbe_gratis::ModifyOperation {
            account,
            op: outbe_gratis::GratisOp::Mint,
            amount,
            op_nonce: 0,
            chain_id: chain,
        },
    );
    if flip_gratis {
        gratis_mac[0] ^= 1;
    }
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
                    chain_id: chain_id(),
                },
            ),
            op_nonce: 0,
        },
        current_gratis_balance: Vec::new(),
        gratis_modify_auth: ModifyAuth {
            mac: gratis_mac,
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

fn rejection(status: &MineGratisFromPromisStatusV1) -> &str {
    match status {
        MineGratisFromPromisStatusV1::PromisRejected { reason }
        | MineGratisFromPromisStatusV1::GratisRejected { reason } => reason,
        MineGratisFromPromisStatusV1::Applied => panic!("expected a rejection"),
    }
}

#[test]
fn applied_promis_records_replays_and_accepts_a_different_attestation_key() {
    let request = mint_request(7);
    let expected = promis_op_canonical_hash(&request);
    let (recorded, tape, leader) = record(|| apply_promis_op(request.clone()).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::PromisOpApplied.value()
    );
    assert!(matches!(recorded.status, PromisOpStatus::Applied));
    assert_eq!(recorded.inputs_canonical_hash, expected);
    let local = prepared(9).attestation_public();
    assert_ne!(leader, local);
    let replayed = replay(leader, tape.clone(), || {
        apply_promis_op(request.clone()).unwrap()
    });
    let verified = with_other_enclave(leader, tape, || apply_promis_op(request).unwrap());
    assert_eq!(replayed, recorded);
    assert_eq!(verified, recorded);
}

#[test]
fn applied_ops_round_trip_on_tx_zero_for_tags_2_and_22() {
    fn round_trip<T: PartialEq + std::fmt::Debug>(expected_tag: u8, call: impl Fn() -> T) {
        let (recorded, tape, leader) = record_tx(&call);
        assert_eq!(tx_payload_of(&tape), expected_tag);
        let _absent = set_in_process_tee_context_for_test(None);
        let replayed = consume_tx(
            WitnessExecutionMode::VerifierReplay,
            leader,
            tape.clone(),
            &call,
        );
        let context = prepared(9);
        assert_ne!(leader, context.attestation_public());
        let _guard = set_in_process_tee_context_for_test(Some(context));
        let verified = consume_tx(
            WitnessExecutionMode::VerifierWithEnclave,
            leader,
            tape,
            &call,
        );
        assert_eq!(replayed, recorded);
        assert_eq!(verified, recorded);
    }

    let promis = mint_request(7);
    round_trip(WitnessResponseTag::PromisOpApplied.value(), move || {
        apply_promis_op(promis.clone()).unwrap()
    });
    let mine = mine_request(9, false, false);
    round_trip(
        WitnessResponseTag::GratisMinedFromPromisV1.value(),
        move || mine_gratis_from_promis(mine.clone()).unwrap(),
    );
}

#[test]
fn replay_of_a_different_promis_request_is_tape_invalid() {
    let first = mint_request(7);
    let second = mint_request(8);
    assert_ne!(
        promis_op_canonical_hash(&first),
        promis_op_canonical_hash(&second)
    );
    let (_recorded, tape, leader) = record(|| apply_promis_op(first).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::PromisOpApplied.value()
    );
    replay_rejects(leader, tape, || apply_promis_op(second));
}

#[test]
fn foreign_promis_chain_records_a_signed_tag_2_rejection() {
    let (_guard, leader) = install(4);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let mut request = mint_request(7);
    request.chain_id = B256::repeat_byte(0x5a);
    let result = apply_promis_op(request).expect("signed rejection");
    let PromisOpStatus::Rejected { reason } = &result.status else {
        panic!("foreign chain was applied");
    };
    assert_eq!(reason, "invalid modify authorization");
    verify_promis_op_attestation(
        &leader,
        result.inputs_canonical_hash,
        &result,
        &result.attestation_tag,
    )
    .unwrap();
    let witness = state.lock().expect("witness lock");
    assert_eq!(witness.staged.len(), 1);
    assert_eq!(
        payload_of(&EnclaveWitnessTapeV1 {
            prefix: witness.staged.clone(),
            txs: Vec::new(),
        }),
        WitnessResponseTag::PromisOpApplied.value()
    );
    assert_eq!(witness.prefix_cursor, 0);
    assert!(witness.candidate_rejection.is_none());
    assert!(witness.first_witness_fault.is_none());
}

#[test]
fn applied_and_rejected_mines_record_tag_22() {
    let applied = mine_request(9, false, false);
    let (mined, tape, leader) = record(|| mine_gratis_from_promis(applied.clone()).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::GratisMinedFromPromisV1.value()
    );
    assert_eq!(mined.status, MineGratisFromPromisStatusV1::Applied);
    let local = prepared(9).attestation_public();
    assert_ne!(leader, local);
    let replayed = replay(leader, tape.clone(), || {
        mine_gratis_from_promis(applied.clone()).unwrap()
    });
    let verified = with_other_enclave(leader, tape, || mine_gratis_from_promis(applied).unwrap());
    assert_eq!(replayed, mined);
    assert_eq!(verified, mined);

    let refused = mine_request(9, true, false);
    let (rejected, tape, leader) = record(|| mine_gratis_from_promis(refused.clone()).unwrap());
    assert_eq!(rejection(&rejected.status), "invalid modify authorization");
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::GratisMinedFromPromisV1.value()
    );
    verify_mine_gratis_from_promis_attestation(
        &leader,
        rejected.inputs_canonical_hash,
        &rejected,
        &rejected.attestation_tag,
    )
    .unwrap();
    let replayed = replay(leader, tape, || mine_gratis_from_promis(refused).unwrap());
    assert_eq!(replayed, rejected);

    let flipped = mine_request(9, false, true);
    let (gratis_rejected, tape, _leader) =
        record(|| mine_gratis_from_promis(flipped.clone()).unwrap());
    assert!(matches!(
        gratis_rejected.status,
        MineGratisFromPromisStatusV1::GratisRejected { .. }
    ));
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::GratisMinedFromPromisV1.value()
    );
}

#[test]
fn replay_of_a_different_mine_request_is_tape_invalid() {
    let first = mine_request(9, false, false);
    let second = mine_request(10, false, false);
    assert_ne!(
        mine_gratis_from_promis_canonical_hash(&first),
        mine_gratis_from_promis_canonical_hash(&second)
    );
    let (_value, tape, leader) = record(|| mine_gratis_from_promis(first).unwrap());
    assert_eq!(
        payload_of(&tape),
        WitnessResponseTag::GratisMinedFromPromisV1.value()
    );
    replay_rejects(leader, tape, || mine_gratis_from_promis(second));
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

#[test]
fn unactivated_context_is_local_unavailable_for_both_tags() {
    for user_candidate in [false, true] {
        let (error, witness) = classify(user_candidate, dormant(4), || {
            apply_promis_op(mint_request(7)).unwrap_err()
        });
        let message = unavailable_message(error);
        assert!(
            message.contains("resident offer material unavailable"),
            "{message}"
        );
        assert!(witness.staged.is_empty());
        assert_eq!(witness.prefix_cursor, 0);
        assert!(witness.candidate_rejection.is_none());
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::LocalUnavailable)
        );

        let (error, witness) = classify(user_candidate, dormant(4), || {
            mine_gratis_from_promis(mine_request(9, false, false)).unwrap_err()
        });
        let message = unavailable_message(error);
        assert!(
            message.contains("resident offer material unavailable"),
            "{message}"
        );
        assert!(witness.staged.is_empty());
        assert_eq!(witness.prefix_cursor, 0);
        assert!(witness.candidate_rejection.is_none());
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::LocalUnavailable)
        );
    }
}

#[test]
fn wrong_payload_tags_are_tape_invalid_before_the_cursor_moves() {
    let promis = mint_request(7);
    let promis_tape = EnclaveWitnessTapeV1 {
        prefix: vec![EnclaveWitnessEntryV1::Payload {
            payload: vec![WitnessResponseTag::GratisOpApplied.value()],
            inputs_canonical_hash: promis_op_canonical_hash(&promis),
            attestation_tag: vec![0; 64],
        }],
        txs: Vec::new(),
    };
    assert_wrong_tag(
        promis_tape,
        || apply_promis_op(promis),
        "witness payload tag 1 does not match expected 2",
    );

    let mine = mine_request(9, false, false);
    let mine_tape = EnclaveWitnessTapeV1 {
        prefix: vec![EnclaveWitnessEntryV1::Payload {
            payload: vec![WitnessResponseTag::PromisOpApplied.value()],
            inputs_canonical_hash: mine_gratis_from_promis_canonical_hash(&mine),
            attestation_tag: vec![0; 64],
        }],
        txs: Vec::new(),
    };
    assert_wrong_tag(
        mine_tape,
        || mine_gratis_from_promis(mine),
        "witness payload tag 2 does not match expected 22",
    );

    let foreign_gratis = encode_tagged_payload(
        WitnessResponseTag::GratisOpApplied,
        &outbe_gratis::GratisOpResult {
            status: outbe_gratis::GratisOpStatus::Applied,
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
    .expect("tag 1 body");
    assert_eq!(
        foreign_gratis[0],
        WitnessResponseTag::GratisOpApplied.value()
    );
    assert_ne!(promis_op_canonical_hash(&mint_request(7)), B256::ZERO);
    assert_wrong_tag(
        zero_hash_tape(foreign_gratis),
        || apply_promis_op(mint_request(7)),
        "witness payload tag 1 does not match expected 2",
    );

    let foreign_promis = encode_tagged_payload(
        WitnessResponseTag::PromisOpApplied,
        &PromisOpResult {
            status: PromisOpStatus::Applied,
            new_balance: Vec::new(),
            event_amount: U256::ZERO,
            next_op_nonce: 0,
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: Vec::new(),
            encrypted_event_amount: Vec::new(),
        },
    )
    .expect("tag 2 body");
    assert_eq!(
        foreign_promis[0],
        WitnessResponseTag::PromisOpApplied.value()
    );
    assert_ne!(
        mine_gratis_from_promis_canonical_hash(&mine_request(9, false, false)),
        B256::ZERO
    );
    assert_wrong_tag(
        zero_hash_tape(foreign_promis),
        || mine_gratis_from_promis(mine_request(9, false, false)),
        "witness payload tag 2 does not match expected 22",
    );
}

fn zero_hash_tape(payload: Vec<u8>) -> EnclaveWitnessTapeV1 {
    EnclaveWitnessTapeV1 {
        prefix: vec![EnclaveWitnessEntryV1::Payload {
            payload,
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: vec![0; 64],
        }],
        txs: Vec::new(),
    }
}

fn assert_wrong_tag<T: std::fmt::Debug>(
    tape: EnclaveWitnessTapeV1,
    call: impl FnOnce() -> Result<T, PrecompileError>,
    expected: &str,
) {
    let _guard = set_in_process_tee_context_for_test(None);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some([0x44; 32]),
        tape,
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let error = call().unwrap_err();
    let sentence = format!("{error}");
    assert!(sentence.contains(expected), "{sentence}");
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

struct RpcRound {
    _guard: InProcessTestGuard,
    state: Arc<Mutex<WitnessState>>,
    _scope: WitnessExecutionScope,
    leader: [u8; 32],
    first: Arc<InProcessTeeContext>,
    second: Arc<InProcessTeeContext>,
}

fn rpc_round() -> RpcRound {
    let first = prepared(4);
    let leader = first.attestation_public();
    let guard = set_in_process_tee_context_for_test(Some(Arc::clone(&first)));
    let second = prepared(9);
    assert_ne!(leader, second.attestation_public());
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::RpcDirect,
        Some(second.attestation_public()),
        EnclaveWitnessTapeV1::default(),
    )));
    let scope = WitnessExecutionScope::enter(state.clone());
    RpcRound {
        _guard: guard,
        state,
        _scope: scope,
        leader,
        first,
        second,
    }
}

#[test]
fn rpc_direct_checks_tags_2_and_22_before_and_after_a_context_swap() {
    let round = rpc_round();
    let promis = mint_request(7);
    let promis_hash = promis_op_canonical_hash(&promis);
    let signed = apply_promis_op(promis.clone()).unwrap();
    verify_promis_op_attestation(&round.leader, promis_hash, &signed, &signed.attestation_tag)
        .unwrap();
    assert!(verify_promis_op_attestation(
        &round.second.attestation_public(),
        promis_hash,
        &signed,
        &signed.attestation_tag,
    )
    .is_err());
    let rejected = apply_promis_op(zero_request()).unwrap();
    assert_eq!(
        match &rejected.status {
            PromisOpStatus::Rejected { reason } => reason.as_str(),
            PromisOpStatus::Applied => panic!("zero amount was applied"),
        },
        "amount must be positive"
    );

    let mine = mine_request(9, false, false);
    let mine_hash = mine_gratis_from_promis_canonical_hash(&mine);
    let mined = mine_gratis_from_promis(mine.clone()).unwrap();
    verify_mine_gratis_from_promis_attestation(
        &round.leader,
        mine_hash,
        &mined,
        &mined.attestation_tag,
    )
    .unwrap();
    assert!(verify_mine_gratis_from_promis_attestation(
        &round.second.attestation_public(),
        mine_hash,
        &mined,
        &mined.attestation_tag,
    )
    .is_err());

    replace_in_process_tee_context(&round.first, Arc::clone(&round.second), || {
        Ok::<(), InProcessDispatchError>(())
    })
    .unwrap();
    assert_eq!(
        in_process_tee_context()
            .expect("successor")
            .attestation_public(),
        round.second.attestation_public()
    );
    let replaced = apply_promis_op(promis).unwrap();
    verify_promis_op_attestation(
        &round.second.attestation_public(),
        promis_hash,
        &replaced,
        &replaced.attestation_tag,
    )
    .unwrap();
    let replaced_mine = mine_gratis_from_promis(mine).unwrap();
    verify_mine_gratis_from_promis_attestation(
        &round.second.attestation_public(),
        mine_hash,
        &replaced_mine,
        &replaced_mine.attestation_tag,
    )
    .unwrap();
    let witness = round.state.lock().expect("witness lock");
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 0);
}

fn tag2_entry(result: &PromisOpResult) -> EnclaveWitnessEntryV1 {
    let mut cleared = result.clone();
    cleared.attestation_tag.clear();
    let payload =
        encode_tagged_payload(WitnessResponseTag::PromisOpApplied, &cleared).expect("tag 2 body");
    EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash: result.inputs_canonical_hash,
        attestation_tag: result.attestation_tag.clone(),
    }
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

fn resign(context: &InProcessTeeContext, mut result: PromisOpResult) -> PromisOpResult {
    result.attestation_tag.clear();
    let preimage =
        promis_op_attestation_preimage(result.inputs_canonical_hash, &result).expect("preimage");
    result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
    result
}

fn execute(context: Arc<InProcessTeeContext>, request: PromisOpRequest) -> PromisOpResult {
    let _guard = set_in_process_tee_context_for_test(Some(context));
    apply_promis_op(request).expect("promis op")
}

#[test]
fn semantic_divergence_on_tag_2_latches_semantic_mismatch() {
    let request = mint_request(100);
    let leader = prepared(4);
    let honest = execute(Arc::clone(&leader), request.clone());
    let divergent = resign(
        &leader,
        PromisOpResult {
            new_balance: vec![0xaa],
            ..honest
        },
    );
    let verifier = prepared(9);
    let local = execute(Arc::clone(&verifier), request.clone());
    assert!(matches!(divergent.status, PromisOpStatus::Applied));
    assert!(matches!(local.status, PromisOpStatus::Applied));
    assert_eq!(divergent.new_balance, vec![0xaa]);
    assert_ne!(local.new_balance, divergent.new_balance);
    assert_eq!(local.inputs_canonical_hash, divergent.inputs_canonical_hash);
    verify_promis_op_attestation(
        &leader.attestation_public(),
        divergent.inputs_canonical_hash,
        &divergent,
        &divergent.attestation_tag,
    )
    .unwrap();
    verify_promis_op_attestation(
        &verifier.attestation_public(),
        local.inputs_canonical_hash,
        &local,
        &local.attestation_tag,
    )
    .unwrap();
    let state = Arc::new(Mutex::new(tx_witness(
        WitnessExecutionMode::VerifierWithEnclave,
        leader.attestation_public(),
        tx_tape(tag2_entry(&divergent)),
    )));
    let error = {
        let _guard = set_in_process_tee_context_for_test(Some(verifier));
        let _scope = WitnessExecutionScope::enter(state.clone());
        apply_promis_op(request).unwrap_err()
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
fn non_canonical_tag_2_bytes_latch_tape_invalid_at_cursor_zero() {
    let request = mint_request(50);
    let leader_ctx = prepared(4);
    let leader = leader_ctx.attestation_public();
    let honest = execute(leader_ctx, request.clone());
    let mut clean = honest.clone();
    clean.attestation_tag.clear();
    let mut trailing = encode_tagged_payload(WitnessResponseTag::PromisOpApplied, &clean).unwrap();
    trailing.push(0x00);
    let unstripped = encode_tagged_payload(
        WitnessResponseTag::PromisOpApplied,
        &PromisOpResult {
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
                set_in_process_tee_context_for_test(Some(prepared(9)))
            } else {
                set_in_process_tee_context_for_test(None)
            };
            let _scope = WitnessExecutionScope::enter(state.clone());
            assert!(apply_promis_op(request.clone()).is_err());
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

#[test]
fn recorded_tag_2_entry_matches_an_independent_encode() {
    let request = mint_request(1);
    let context = prepared(4);
    let leader = context.attestation_public();
    let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&context)));
    let state = Arc::new(Mutex::new(tx_witness(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    )));
    let recorded = {
        let _scope = WitnessExecutionScope::enter(state.clone());
        apply_promis_op(request.clone()).unwrap()
    };
    let witness = state.lock().expect("witness lock");
    assert!(witness.first_witness_fault.is_none());
    assert_eq!(witness.staged.len(), 1);
    let entry = witness.staged[0].clone();
    drop(witness);
    let direct = apply_promis_op(request).unwrap();
    assert_eq!(direct, recorded);
    let reference = tag2_entry(&direct);
    assert_eq!(entry, reference);
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &entry
    else {
        panic!("recorded entry is not a payload");
    };
    assert_eq!(payload[0], WitnessResponseTag::PromisOpApplied.value());
    assert_eq!(*inputs_canonical_hash, direct.inputs_canonical_hash);
    assert_eq!(attestation_tag, &direct.attestation_tag);
    let mut cleared = direct.clone();
    cleared.attestation_tag.clear();
    let decoded: PromisOpResult =
        decode_tagged_payload(WitnessResponseTag::PromisOpApplied, payload).unwrap();
    assert_eq!(decoded, cleared);
}
