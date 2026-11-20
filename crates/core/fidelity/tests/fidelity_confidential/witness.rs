//! Record, replay, and a second enclave for snapshot and query.
//!
//! Replay runs with no resident context. A live execute would fail closed.
//! The second enclave uses another attestation key and the same offer material.

use std::sync::{Arc, Mutex};

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolError;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use outbe_core_common::confidential_tee::WitnessResponseTag;
use outbe_fidelity::enclave_client::{apply_cohort_op, query_index, snapshot_leagues};
use outbe_fidelity::precompile::IFidelity;
use outbe_fidelity::{
    eip191_hash, fidelity_cohort_canonical_hash, fidelity_query_auth_message,
    fidelity_query_canonical_hash, fidelity_snapshot_canonical_hash,
    verify_fidelity_cohort_attestation, verify_fidelity_query_attestation, FidelityCohortOp,
    FidelityCohortRequest, FidelityOpSection, FidelityQueryRequest, FidelitySnapshotEntry,
    FidelitySnapshotRequest,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_tee::in_process_dispatch::{
    in_process_tee_context, replace_in_process_tee_context, set_in_process_tee_context_for_test,
};
use outbe_tee::resident::ResidentOfferMaterial;
use outbe_tee::witness::{
    BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode,
    WitnessExecutionScope, WitnessState,
};
use outbe_tee::InProcessDispatchError;
use outbe_tee::TransportError;

const GROUP: &[u8] = b"fidelity-witness-group-signature-v1!!!!";

fn binding() -> NetworkBindingV1 {
    NetworkBindingV1 {
        chain_id: U256::from(676).to_be_bytes(),
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

fn install(seed: u8) -> (outbe_tee::in_process_dispatch::InProcessTestGuard, [u8; 32]) {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    context.activate_for_test(offer()).expect("activate offer");
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
    let context = Arc::new(outbe_tee::test_utils::resident_context(9, binding()));
    context
        .activate_for_test(offer())
        .expect("activate verifier offer");
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

fn payload_of(tape: &EnclaveWitnessTapeV1) -> (u8, B256, usize) {
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
    (payload[0], *inputs_canonical_hash, payload.len())
}

fn cohort_request() -> FidelityCohortRequest {
    FidelityCohortRequest {
        chain_id: chain_id(),
        account: Address::repeat_byte(0x11),
        amount: U256::ZERO,
        section: FidelityOpSection {
            op: FidelityCohortOp::Probe,
            timestamp: 1_000_000,
            first_qualified_start: 0,
            current_blob: Vec::new(),
        },
    }
}

fn snapshot_request() -> FidelitySnapshotRequest {
    FidelitySnapshotRequest {
        timestamp: 1_000_000,
        first_qualified_start: 0,
        entries: vec![FidelitySnapshotEntry {
            owner: Address::repeat_byte(0x11),
            cohort_blob: Vec::new(),
        }],
    }
}

fn account_of(seed: u8) -> (k256::ecdsa::SigningKey, Address) {
    let signer = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("signer");
    let point = signer.verifying_key().to_encoded_point(false);
    let address = Address::from_slice(&keccak256(&point.as_bytes()[1..])[12..]);
    (signer, address)
}

fn owner_sig(signer: &k256::ecdsa::SigningKey, account: Address, expiry: u64) -> Vec<u8> {
    let prehash = eip191_hash(&fidelity_query_auth_message(chain_id(), account, expiry));
    let (signature, id) = signer
        .sign_prehash(prehash.as_slice())
        .expect("owner signature");
    let mut bytes = [0u8; 65];
    bytes[..64].copy_from_slice(&signature.to_bytes());
    bytes[64] = id.to_byte();
    bytes.to_vec()
}

fn query_request(account: Address, owner_sig: Vec<u8>) -> FidelityQueryRequest {
    FidelityQueryRequest {
        chain_id: chain_id(),
        account,
        cohort_blob: Vec::new(),
        query_timestamp: 1_000_000,
        block_timestamp: 1_500_000,
        first_qualified_start: 0,
        expiry: 2_000_000,
        owner_sig,
    }
}

fn unavailable(error: PrecompileError) -> String {
    match error {
        PrecompileError::EnclaveUnavailable(message) => message,
        other => panic!("expected EnclaveUnavailable, got {other:?}"),
    }
}

#[test]
fn transport_errors_keep_the_historical_host_sentences() {
    use outbe_fidelity::enclave_client::{map_consensus_error, unexpected_answer};
    for operation in [
        "ApplyFidelityCohortOp",
        "SnapshotFidelityLeagues",
        "QueryFidelityIndex",
    ] {
        let boom = PrecompileError::from(map_consensus_error(
            operation,
            TransportError::enclave_error(std::io::Error::other("boom")),
        ));
        assert_eq!(
            unavailable(boom),
            format!("enclave {operation} error: boom")
        );
        let unexpected = PrecompileError::from(unexpected_answer(operation));
        assert!(unavailable(unexpected).starts_with(&format!(
            "tee_sidecar_unavailable: {operation}: codec error: "
        )));
    }
}

fn rejection_reason(error: PrecompileError) -> String {
    let PrecompileError::RevertBytes(bytes) = error else {
        panic!("query rejection was not a revert");
    };
    IFidelity::QueryFailed::abi_decode(&bytes)
        .expect("QueryFailed")
        .reason
}

#[test]
fn snapshot_records_replays_and_accepts_a_different_attestation_key() {
    let request = snapshot_request();
    let expected = fidelity_snapshot_canonical_hash(&request);
    let (recorded, tape, leader) = record(|| snapshot_leagues(request.clone()).unwrap());
    let (tag, hash, len) = payload_of(&tape);
    assert_eq!(tag, WitnessResponseTag::FidelityLeaguesSnapshotted.value());
    assert_eq!(hash, expected);
    assert!(len > 1);
    assert_eq!(recorded[0].owner, Address::repeat_byte(0x11));
    let replayed = replay(leader, tape.clone(), || {
        snapshot_leagues(request.clone()).unwrap()
    });
    let verified = with_other_enclave(leader, tape, || snapshot_leagues(request).unwrap());
    assert_eq!(replayed, recorded);
    assert_eq!(verified, recorded);
}

#[test]
fn query_records_replays_and_keeps_the_leader_answer() {
    let (signer, account) = account_of(0x33);
    let request = query_request(account, owner_sig(&signer, account, 2_000_000));
    let expected = fidelity_query_canonical_hash(&request);
    let (recorded, tape, leader) = record(|| query_index(request.clone()).unwrap());
    let (tag, hash, _) = payload_of(&tape);
    assert_eq!(tag, WitnessResponseTag::FidelityIndexQueried.value());
    assert_eq!(hash, expected);
    assert_eq!(recorded.inputs_canonical_hash, expected);
    assert_eq!(recorded.attestation_tag.len(), 64);
    let replayed = replay(leader, tape.clone(), || {
        query_index(request.clone()).unwrap()
    });
    let verified = with_other_enclave(leader, tape, || query_index(request).unwrap());
    assert_eq!(replayed, recorded);
    assert_eq!(verified, recorded);
}

#[test]
fn rejected_query_records_replays_and_accepts_a_different_attestation_key() {
    let request = query_request(Address::repeat_byte(0x11), Vec::new());
    let expected = fidelity_query_canonical_hash(&request);
    let (recorded, tape, leader) = record(|| query_index(request.clone()).unwrap_err());
    let (tag, hash, _) = payload_of(&tape);
    assert_eq!(tag, WitnessResponseTag::FidelityQueryRejected.value());
    assert_eq!(hash, expected);
    let reason = rejection_reason(recorded);
    assert_eq!(reason, "fidelity: owner signature must be 65 bytes");
    let replayed = replay(leader, tape.clone(), || {
        query_index(request.clone()).unwrap_err()
    });
    let verified = with_other_enclave(leader, tape, || query_index(request).unwrap_err());
    assert_eq!(rejection_reason(replayed), reason);
    assert_eq!(rejection_reason(verified), reason);
}

#[test]
fn rpc_direct_checks_the_executing_context_and_ignores_a_later_swap() {
    let first = Arc::new(outbe_tee::test_utils::resident_context(4, binding()));
    first.activate_for_test(offer()).expect("activate offer");
    let leader = first.attestation_public();
    let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&first)));
    let second = Arc::new(outbe_tee::test_utils::resident_context(9, binding()));
    second
        .activate_for_test(offer())
        .expect("activate successor offer");
    assert_ne!(leader, second.attestation_public());
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::RpcDirect,
        Some(second.attestation_public()),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    assert!(outbe_tee::call_context::current_witness_state().is_some());

    let cohort = cohort_request();
    let snapshot = snapshot_request();
    let (signer, account) = account_of(0x33);
    let query = query_request(account, owner_sig(&signer, account, 2_000_000));
    let cohort_hash = fidelity_cohort_canonical_hash(&cohort);
    let query_hash = fidelity_query_canonical_hash(&query);

    let cohort_result = apply_cohort_op(cohort.clone()).unwrap();
    let leagues = snapshot_leagues(snapshot.clone()).unwrap();
    let queried = query_index(query.clone()).unwrap();
    assert_eq!(leagues[0].owner, Address::repeat_byte(0x11));
    assert_eq!(cohort_result.inputs_canonical_hash, cohort_hash);
    assert_eq!(queried.inputs_canonical_hash, query_hash);
    verify_fidelity_cohort_attestation(
        &leader,
        cohort_hash,
        &cohort_result,
        &cohort_result.attestation_tag,
    )
    .unwrap();
    let mut swapped_cohort = cohort.clone();
    swapped_cohort.section.timestamp += 1;
    let substituted = verify_fidelity_cohort_attestation(
        &leader,
        fidelity_cohort_canonical_hash(&swapped_cohort),
        &cohort_result,
        &cohort_result.attestation_tag,
    )
    .expect_err("substituted cohort request");
    assert!(matches!(
        substituted,
        TransportError::AppAttestation { descriptor, .. }
            if descriptor == outbe_fidelity::FIDELITY_ATTESTATION
    ));
    verify_fidelity_query_attestation(&leader, query_hash, &queried, &queried.attestation_tag)
        .unwrap();
    let mut flipped = cohort_result.clone();
    flipped.attestation_tag[0] ^= 0xff;
    assert!(verify_fidelity_cohort_attestation(
        &leader,
        cohort_hash,
        &flipped,
        &flipped.attestation_tag
    )
    .is_err());
    assert!(verify_fidelity_query_attestation(
        &leader,
        B256::ZERO,
        &queried,
        &queried.attestation_tag
    )
    .is_err());
    assert!(verify_fidelity_cohort_attestation(
        &second.attestation_public(),
        cohort_hash,
        &cohort_result,
        &cohort_result.attestation_tag,
    )
    .is_err());

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
    let replaced = apply_cohort_op(cohort).unwrap();
    assert!(snapshot_leagues(snapshot).is_ok());
    assert!(query_index(query).is_ok());
    let rejected = query_index(query_request(Address::repeat_byte(0x44), Vec::new())).unwrap_err();
    assert_eq!(
        rejection_reason(rejected),
        "fidelity: owner signature must be 65 bytes"
    );
    verify_fidelity_cohort_attestation(
        &second.attestation_public(),
        fidelity_cohort_canonical_hash(&cohort_request()),
        &replaced,
        &replaced.attestation_tag,
    )
    .unwrap();
    assert!(verify_fidelity_cohort_attestation(
        &leader,
        replaced.inputs_canonical_hash,
        &replaced,
        &replaced.attestation_tag,
    )
    .is_err());

    let witness = state.lock().expect("witness lock");
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 0);
}

#[test]
fn replay_of_a_different_request_stops_before_the_tape_cursor_moves() {
    let snapshot_a = snapshot_request();
    let mut snapshot_b = snapshot_a.clone();
    snapshot_b.timestamp += 1;
    assert_ne!(
        fidelity_snapshot_canonical_hash(&snapshot_a),
        fidelity_snapshot_canonical_hash(&snapshot_b)
    );
    let (_recorded, tape, leader) = record(|| snapshot_leagues(snapshot_a).unwrap());
    assert_eq!(
        payload_of(&tape).0,
        WitnessResponseTag::FidelityLeaguesSnapshotted.value()
    );
    replay_rejects(leader, tape, || snapshot_leagues(snapshot_b));

    let (signer, account) = account_of(0x33);
    let applied_a = query_request(account, owner_sig(&signer, account, 2_000_000));
    let applied_b = query_request(account, owner_sig(&signer, account, 2_000_001));
    assert_ne!(
        fidelity_query_canonical_hash(&applied_a),
        fidelity_query_canonical_hash(&applied_b)
    );
    let (_recorded, tape, leader) = record(|| query_index(applied_a).unwrap());
    assert_eq!(
        payload_of(&tape).0,
        WitnessResponseTag::FidelityIndexQueried.value()
    );
    replay_rejects(leader, tape, || query_index(applied_b));

    let rejected_a = query_request(Address::repeat_byte(0x11), Vec::new());
    let rejected_b = query_request(Address::repeat_byte(0x22), Vec::new());
    assert_ne!(
        fidelity_query_canonical_hash(&rejected_a),
        fidelity_query_canonical_hash(&rejected_b)
    );
    let (_recorded, tape, leader) = record(|| query_index(rejected_a).unwrap_err());
    assert_eq!(
        payload_of(&tape).0,
        WitnessResponseTag::FidelityQueryRejected.value()
    );
    replay_rejects(leader, tape, || query_index(rejected_b));
}
