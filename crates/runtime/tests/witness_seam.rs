//! Cross-domain witness refusals, and the account-query / canary power gate.
//!
//! Two linked executor blocks live in `evm_executor/witness_two_heights.rs`.
//! This file keeps the resident-key, authority-gate, and unsigned-refusal oracles.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    EnclaveClientError, LocalCryptoError, TributeSourceError, WitnessResponseTag, WitnessSchema,
    FIDELITY_ATTESTATION, GRATIS_OP_ATTESTATION, NOD_MINT_REJECTED, PROMIS_OP_ATTESTATION,
    TRIBUTE_OFFER_ATTESTATION, TRIBUTE_SOURCE_REJECTED,
};
use outbe_core_common::nod_encryption::NodTermsV1;
use outbe_fidelity::enclave_client::snapshot_leagues;
use outbe_fidelity::{FidelitySnapshotEntry, FidelitySnapshotRequest};
use outbe_gratis::enclave_client::apply_gratis_op;
use outbe_gratis::nod_mine::EncryptedNodMintRejectedV1;
use outbe_gratis::{
    derive_account_keys as gratis_keys, modify_mac as gratis_mac, GratisOp, GratisOpRequest,
    ModifyAuth, ModifyOperation as GratisModify,
};
use outbe_nod::nod_encryption::encrypt_nod;
use outbe_nod::nod_host::read_nod_amount;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_promis::enclave_client::apply_promis_op;
use outbe_promis::{
    derive_account_keys as promis_keys, modify_mac as promis_mac, ModifyOperation as PromisModify,
    PromisOp, PromisOpRequest,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::resident::ResidentOfferMaterial;
use outbe_tee::witness::{
    BlockWitnessFault, CandidateRejection, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1,
    TxWitnessV1, WitnessExecutionMode, WitnessExecutionScope, WitnessPolicy, WitnessState,
};
use outbe_tee::{TeeAttestationError, TransportError};
use outbe_tribute::confidential::host::{
    apply_day_operation, process_encrypted_offer_batch, read_day_amount,
};
use outbe_tribute::confidential::{TributeDayOpRequestV2, TributeDayOperationV2};

#[path = "witness_seam/account_query.rs"]
mod account_query;

const GROUP: &[u8] = b"runtime-witness-seam-group-signature!!";
const CHAIN: u64 = 1;

struct Prepared {
    context: Arc<InProcessTeeContext>,
    gratis: GratisOpRequest,
    promis: PromisOpRequest,
    fidelity: FidelitySnapshotRequest,
    nod: outbe_core_common::nod_encryption::EncryptedNodV1,
}

fn binding() -> NetworkBindingV1 {
    NetworkBindingV1 {
        chain_id: U256::from(CHAIN).to_be_bytes(),
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

fn context(seed: u8, active: bool) -> Arc<InProcessTeeContext> {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    if active {
        context.activate_for_test(offer()).expect("activate");
    }
    context
}

fn account() -> Address {
    Address::repeat_byte(0x11)
}

fn gratis_request(balance: Vec<u8>, nonce: u64) -> GratisOpRequest {
    let keys = gratis_keys(
        &context(4, true)
            .state_key_handle(outbe_core_common::confidential_tee::GRATIS.state_info(), 0)
            .expect("gratis key"),
        account(),
    )
    .expect("gratis account keys");
    let operation = GratisModify {
        account: account(),
        op: GratisOp::Mint,
        amount: U256::from(7u64),
        op_nonce: nonce,
        chain_id: chain_id(),
    };
    GratisOpRequest {
        op: GratisOp::Mint,
        chain_id: chain_id(),
        account: account(),
        amount: U256::from(7u64),
        current_balance: balance,
        current_pledged: Vec::new(),
        modify_auth: ModifyAuth {
            mac: gratis_mac(&keys, &operation),
            op_nonce: nonce,
        },
        fidelity: None,
    }
}

fn promis_request() -> PromisOpRequest {
    let keys = promis_keys(
        &context(4, true)
            .state_key_handle(outbe_core_common::confidential_tee::PROMIS.state_info(), 0)
            .expect("promis key"),
        account(),
    )
    .expect("promis account keys");
    let operation = PromisModify {
        account: account(),
        op: PromisOp::Mint,
        amount: U256::ONE,
        op_nonce: 0,
        chain_id: chain_id(),
    };
    PromisOpRequest {
        op: PromisOp::Mint,
        chain_id: chain_id(),
        account: account(),
        amount: U256::ONE,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth {
            mac: promis_mac(&keys, &operation),
            op_nonce: 0,
        },
    }
}

fn fidelity_request() -> FidelitySnapshotRequest {
    FidelitySnapshotRequest {
        timestamp: 1_000_000,
        first_qualified_start: 0,
        entries: vec![FidelitySnapshotEntry {
            owner: account(),
            cohort_blob: Vec::new(),
        }],
    }
}

fn nod() -> outbe_core_common::nod_encryption::EncryptedNodV1 {
    let key = context(4, true)
        .state_key_handle(outbe_core_common::confidential_tee::NOD.state_info(), 0)
        .expect("nod key");
    encrypt_nod(
        &key,
        NodTermsV1 {
            chain_id: CHAIN,
            nod_id: U256::ONE,
            owner: account(),
            emission_day: 20250115,
            league_id: 2,
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 840,
        },
        U256::from(1_000_000u64),
    )
    .expect("encrypt nod")
}

fn prepared() -> Prepared {
    Prepared {
        context: context(4, true),
        gratis: gratis_request(Vec::new(), 0),
        promis: promis_request(),
        fidelity: fidelity_request(),
        nod: nod(),
    }
}

fn enter(
    mode: WitnessExecutionMode,
    leader: [u8; 32],
    tape: EnclaveWitnessTapeV1,
) -> (Arc<Mutex<WitnessState>>, WitnessExecutionScope) {
    let state = Arc::new(Mutex::new(WitnessState::new(mode, Some(leader), tape)));
    let scope = WitnessExecutionScope::enter(Arc::clone(&state));
    (state, scope)
}

#[test]
fn inactive_resident_key_is_local_unavailable_in_every_domain() {
    let cold = context(3, false);
    let leader = cold.attestation_public();
    let _guard = set_in_process_tee_context_for_test(Some(cold));
    let sample = prepared();
    latch(leader, "gratis", || {
        drop(apply_gratis_op(sample.gratis.clone()))
    });
    latch(leader, "promis", || {
        drop(apply_promis_op(sample.promis.clone()))
    });
    latch(leader, "fidelity", || {
        drop(snapshot_leagues(sample.fidelity.clone()))
    });
    latch(leader, "nod", || drop(read_nod_amount(&sample.nod)));
    latch(leader, "tribute", || {
        drop(process_encrypted_offer_batch(CHAIN, Vec::new()))
    });
}

fn latch(leader: [u8; 32], domain: &str, call: impl FnOnce()) {
    let (state, _scope) = enter(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    );
    call();
    assert_eq!(
        state.lock().expect("witness lock").first_witness_fault,
        Some(BlockWitnessFault::LocalUnavailable),
        "{domain}"
    );
}

#[test]
fn active_resident_chain_mismatch_is_unsigned_on_a_user_tx_and_tape_invalid_on_prefix() {
    let _policy = ClearWitnessPolicy;
    outbe_tee::witness::set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));
    let hot = context(4, true);
    let leader = hot.attestation_public();
    let _runtime = set_in_process_tee_context_for_test(Some(hot));

    let (user_state, _user_scope) = enter(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    );
    {
        let mut guard = user_state.lock().expect("witness lock");
        guard.tx_index = Some(0);
        guard.is_user_candidate = true;
    }
    let user_error =
        process_encrypted_offer_batch(778, Vec::new()).expect_err("user chain mismatch");
    assert!(!user_error.is_node_local(), "{user_error}");
    let user_guard = user_state.lock().expect("witness lock");
    assert_eq!(
        user_guard.candidate_rejection,
        Some(CandidateRejection::UnsignedEnclaveError)
    );
    assert!(user_guard.staged.is_empty());
    assert_eq!(user_guard.first_witness_fault, None);
    drop(user_guard);
    drop(_user_scope);

    let (prefix_state, _prefix_scope) = enter(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    );
    let prefix_error =
        process_encrypted_offer_batch(778, Vec::new()).expect_err("prefix chain mismatch");
    assert!(!prefix_error.is_node_local(), "{prefix_error}");
    let prefix_guard = prefix_state.lock().expect("witness lock");
    assert_eq!(
        prefix_guard.first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
    assert!(prefix_guard.candidate_rejection.is_none());
    assert!(prefix_guard.staged.is_empty());
}

fn day_freeze(chain_id: u64) -> TributeDayOpRequestV2 {
    TributeDayOpRequestV2 {
        chain_id,
        worldwide_day: outbe_system_common::time::WorldwideDay::new(20_250_115),
        previous: None,
        public_state_hash: B256::ZERO,
        operation: TributeDayOperationV2::Freeze,
    }
}

fn open_tx(
    mode: WitnessExecutionMode,
    leader: [u8; 32],
    tape: EnclaveWitnessTapeV1,
) -> (Arc<Mutex<WitnessState>>, WitnessExecutionScope) {
    let (state, scope) = enter(mode, leader, tape);
    let mut guard = state.lock().expect("witness lock");
    guard.tx_index = Some(0);
    guard.is_user_candidate = true;
    drop(guard);
    (state, scope)
}

#[test]
fn day_write_refuses_a_foreign_chain_and_a_cold_key() {
    let _policy = ClearWitnessPolicy;
    outbe_tee::witness::set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));
    let hot = context(4, true);
    let leader = hot.attestation_public();
    let _runtime = set_in_process_tee_context_for_test(Some(Arc::clone(&hot)));
    let (state, scope) = open_tx(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    );
    record_indexed(&state, 0, 13, || {
        apply_day_operation(day_freeze(CHAIN)).expect("tag 13");
    });
    let tape = state.lock().expect("witness lock").tape.clone();
    drop((scope, _runtime));
    let foreign = resident_on(9, 778, true);
    let cold_key = resident_on(8, CHAIN, false);
    let runs = [
        (
            WitnessExecutionMode::ProposerRecord,
            context(4, true),
            778,
            false,
        ),
        (
            WitnessExecutionMode::VerifierWithEnclave,
            foreign,
            CHAIN,
            false,
        ),
        (
            WitnessExecutionMode::ProposerRecord,
            Arc::clone(&cold_key),
            CHAIN,
            true,
        ),
        (
            WitnessExecutionMode::VerifierWithEnclave,
            cold_key,
            CHAIN,
            true,
        ),
    ];
    for (mode, local, chain_id, cold) in runs {
        let _local = set_in_process_tee_context_for_test(Some(local));
        let use_tape = mode == WitnessExecutionMode::VerifierWithEnclave;
        let recorded = if use_tape {
            tape.clone()
        } else {
            EnclaveWitnessTapeV1::default()
        };
        let (state, _scope) = open_tx(mode, leader, recorded);
        let error = apply_day_operation(day_freeze(chain_id)).expect_err("day write");
        let guard = state.lock().expect("witness lock");
        let proposer = mode == WitnessExecutionMode::ProposerRecord;
        assert!(guard.staged.is_empty());
        if cold {
            assert!(matches!(
                error,
                TransportError::InProcess(outbe_tee::InProcessDispatchError::Resident(
                    outbe_tee::in_process::InProcessError::NotReady
                ))
            ));
            assert!(guard.candidate_rejection.is_none());
        } else {
            assert!(
                matches!(error, TransportError::EnclaveCause { .. }) && !error.is_node_local(),
                "{error}"
            );
            assert!(
                error.to_string().contains("request chain differs"),
                "{error}"
            );
            assert_eq!(
                guard.candidate_rejection,
                proposer.then_some(CandidateRejection::UnsignedEnclaveError)
            );
        }
        assert_eq!(
            guard.first_witness_fault,
            (!proposer || cold).then_some(BlockWitnessFault::LocalUnavailable)
        );
    }
}

#[test]
fn verifier_with_enclave_latches_local_unavailable_on_a_tribute_day_read() {
    let _policy = ClearWitnessPolicy;
    uninstall_domain_standins();
    let leader_ctx = context(4, true);
    let leader = leader_ctx.attestation_public();
    let _runtime = set_in_process_tee_context_for_test(Some(Arc::clone(&leader_ctx)));
    let seeded = apply_day_operation(day_freeze(CHAIN)).expect("seed day amount");
    outbe_tee::witness::set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));
    let (state, scope) = open_tx(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    );
    record_indexed(&state, 0, 14, || {
        assert_eq!(
            read_day_amount(seeded.clone()).expect("leader day read"),
            U256::ZERO
        );
    });
    let tape = state.lock().expect("witness lock").tape.clone();
    drop((scope, _runtime));
    for (label, local) in [
        ("active resident chain mismatch", resident_on(9, 778, true)),
        ("keyless resident", resident_on(9, CHAIN, false)),
    ] {
        let _local = set_in_process_tee_context_for_test(Some(local));
        let (state, _scope) = open_tx(
            WitnessExecutionMode::VerifierWithEnclave,
            leader,
            tape.clone(),
        );
        let error = read_day_amount(seeded.clone()).expect_err(label);
        let guard = state.lock().expect("witness lock");
        assert!(
            guard.candidate_rejection.is_none() && guard.staged.is_empty(),
            "{label}: {error}"
        );
        assert_eq!(
            guard.first_witness_fault,
            Some(BlockWitnessFault::LocalUnavailable),
            "{label}: {error}"
        );
        assert_eq!((guard.replay_cursor, guard.tx_cursor), (0, 0), "{label}");
    }
}

fn resident_on(seed: u8, chain: u64, active: bool) -> Arc<InProcessTeeContext> {
    let binding = NetworkBindingV1 {
        chain_id: U256::from(chain).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
    };
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding));
    if active {
        context
            .activate_for_test(
                ResidentOfferMaterial::for_test(GROUP.to_vec(), B256::from(binding.chain_id), 0, 0)
                    .expect("offer material"),
            )
            .expect("activate");
    }
    context
}

#[test]
fn indexed_user_scopes_record_five_domains_and_match_replay() {
    let _policy = ClearWitnessPolicy;
    uninstall_domain_standins();
    outbe_tee::witness::set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));
    let ready = prepared();
    let leader = ready.context.attestation_public();
    let other = context(9, true);
    assert_ne!(leader, other.attestation_public());
    let _runtime = set_in_process_tee_context_for_test(Some(Arc::clone(&ready.context)));
    let (state, scope) = enter(
        WitnessExecutionMode::ProposerRecord,
        leader,
        EnclaveWitnessTapeV1::default(),
    );
    record_indexed(&state, 0, 1, || {
        let applied = apply_gratis_op(ready.gratis.clone()).expect("gratis");
        assert!(matches!(
            applied.status,
            outbe_gratis::GratisOpStatus::Applied
        ));
    });
    record_indexed(&state, 1, 2, || {
        let applied = apply_promis_op(ready.promis.clone()).expect("promis");
        assert!(matches!(
            applied.status,
            outbe_promis::PromisOpStatus::Applied
        ));
    });
    record_indexed(&state, 2, 4, || {
        let leagues = snapshot_leagues(ready.fidelity.clone()).expect("fidelity");
        assert_eq!(leagues.len(), 1);
    });
    record_indexed(&state, 3, 10, || {
        let amount = read_nod_amount(&ready.nod).expect("nod");
        assert_eq!(amount, U256::from(1_000_000u64));
    });
    record_indexed(&state, 4, 11, || {
        let offers = process_encrypted_offer_batch(CHAIN, Vec::new()).expect("tribute");
        assert!(offers.is_empty());
    });
    let tape = state.lock().expect("witness lock").tape.clone();
    drop(scope);
    drop(_runtime);
    assert!(tape.prefix.is_empty());
    assert_eq!(tape.txs.len(), 5);
    for (index, tx) in tape.txs.iter().enumerate() {
        assert_eq!(tx.tx_index, index as u32);
        assert_eq!(tx.entries.len(), 1);
    }

    let _verifier = set_in_process_tee_context_for_test(Some(other));
    let (state, scope) = enter(
        WitnessExecutionMode::VerifierWithEnclave,
        leader,
        tape.clone(),
    );
    replay_indexed(&state, 0, || {
        apply_gratis_op(ready.gratis.clone()).expect("gratis enclave");
    });
    replay_indexed(&state, 1, || {
        apply_promis_op(ready.promis.clone()).expect("promis enclave");
    });
    replay_indexed(&state, 2, || {
        snapshot_leagues(ready.fidelity.clone()).expect("fidelity enclave");
    });
    replay_indexed(&state, 3, || {
        read_nod_amount(&ready.nod).expect("nod enclave");
    });
    replay_indexed(&state, 4, || {
        process_encrypted_offer_batch(CHAIN, Vec::new()).expect("tribute enclave");
    });
    assert!(state
        .lock()
        .expect("witness lock")
        .first_witness_fault
        .is_none());
    drop(scope);
    drop(_verifier);

    outbe_tee::witness::set_witness_policy_for_test(Some(WitnessPolicy::ReplayOnly));
    let _absent = set_in_process_tee_context_for_test(None);
    let (state, scope) = enter(WitnessExecutionMode::VerifierReplay, leader, tape);
    replay_indexed(&state, 0, || {
        apply_gratis_op(ready.gratis.clone()).expect("gratis replay");
    });
    replay_indexed(&state, 1, || {
        apply_promis_op(ready.promis.clone()).expect("promis replay");
    });
    replay_indexed(&state, 2, || {
        snapshot_leagues(ready.fidelity.clone()).expect("fidelity replay");
    });
    replay_indexed(&state, 3, || {
        read_nod_amount(&ready.nod).expect("nod replay");
    });
    replay_indexed(&state, 4, || {
        process_encrypted_offer_batch(CHAIN, Vec::new()).expect("tribute replay");
    });
    assert!(state
        .lock()
        .expect("witness lock")
        .first_witness_fault
        .is_none());
    drop(scope);
}

fn record_indexed(state: &Arc<Mutex<WitnessState>>, index: u32, tag: u8, call: impl FnOnce()) {
    {
        let mut guard = state.lock().expect("witness lock");
        guard.tx_index = Some(index);
        guard.is_user_candidate = true;
        guard.staged.clear();
    }
    call();
    let mut guard = state.lock().expect("witness lock");
    assert!(guard.first_witness_fault.is_none(), "tx {index}");
    assert!(guard.candidate_rejection.is_none(), "tx {index}");
    assert_eq!(guard.staged.len(), 1, "tx {index}");
    match &guard.staged[0] {
        EnclaveWitnessEntryV1::Payload { payload, .. } => assert_eq!(payload.first(), Some(&tag)),
        other => panic!("tx {index} recorded {other:?}"),
    }
    let entries = std::mem::take(&mut guard.staged);
    guard.tape.txs.push(TxWitnessV1 {
        tx_index: index,
        entries,
    });
}

fn replay_indexed(state: &Arc<Mutex<WitnessState>>, index: u32, call: impl FnOnce()) {
    {
        let mut guard = state.lock().expect("witness lock");
        guard.tx_index = Some(index);
        guard.is_user_candidate = false;
        guard.replay_cursor = 0;
    }
    call();
    let mut guard = state.lock().expect("witness lock");
    assert!(
        guard.first_witness_fault.is_none(),
        "tx {index}: {:?}",
        guard.first_witness_fault
    );
    assert_eq!(
        guard.replay_cursor, 1,
        "tx {index} did not consume its entry"
    );
    guard.tx_cursor += 1;
    guard.replay_cursor = 0;
}

fn uninstall_domain_standins() {
    outbe_gratis::enclave_client::test_enclave::uninstall();
    outbe_fidelity::enclave_client::test_enclave::uninstall();
    // Nod's stand-in is crate-private and starts uninstalled.
    outbe_nod::test_support::uninstall();
    outbe_tribute::enclave_client::test_enclave::uninstall();
}

struct ClearWitnessPolicy;

impl Drop for ClearWitnessPolicy {
    fn drop(&mut self) {
        outbe_tee::witness::set_witness_policy_for_test(None);
    }
}

#[derive(Debug)]
struct Reason(&'static str);

impl std::fmt::Display for Reason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Reason {}

#[test]
fn metric_classes_and_local_crypto_error_messages_remain_exact() {
    let bad_sig = || TeeAttestationError::InvalidSignatureLength { actual: 3 };
    for (descriptor, expected) in [
        (TRIBUTE_OFFER_ATTESTATION, "tribute_offer_attestation"),
        (GRATIS_OP_ATTESTATION, "gratis_op_attestation"),
        (PROMIS_OP_ATTESTATION, "promis_op_attestation"),
        (FIDELITY_ATTESTATION, "fidelity_attestation"),
    ] {
        assert_eq!(
            TransportError::app_attestation(descriptor, bad_sig()).metric_class(),
            expected
        );
    }
    for (descriptor, expected) in [
        (TRIBUTE_SOURCE_REJECTED, "tribute_source_rejected"),
        (NOD_MINT_REJECTED, "nod_mint_rejected"),
    ] {
        assert_eq!(
            TransportError::app_rejected(descriptor, Reason("rejected")).metric_class(),
            expected
        );
    }
    let source =
        TransportError::app_rejected(TRIBUTE_SOURCE_REJECTED, TributeSourceError::ContextMismatch);
    assert_eq!(source.metric_class(), "tribute_source_rejected");
    assert_eq!(
        source.to_string(),
        "Tribute source rejected: Tribute source context mismatch"
    );
    let rejected = EncryptedNodMintRejectedV1 {
        reason: "bad nonce".into(),
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    };
    assert_eq!(
        <EncryptedNodMintRejectedV1 as WitnessSchema>::TAG,
        WitnessResponseTag::EncryptedNodMintRejectedV1
    );
    let mint = TransportError::app_rejected(NOD_MINT_REJECTED, Reason("bad nonce"));
    assert_eq!(mint.metric_class(), "nod_mint_rejected");
    assert_eq!(
        mint.to_string(),
        format!("encrypted NOD mint rejected: {}", rejected.reason)
    );
    assert_local_crypto_sentences();
    assert_eq!(
        EnclaveClientError::TributeSourceRejected(TributeSourceError::ContextMismatch).to_string(),
        "Tribute source rejected: Tribute source context mismatch"
    );
}

#[test]
fn witness_consensus_call_enclave_cause_does_not_latch_local_unavailable() {
    let context = Arc::new(outbe_tee::test_utils::resident_context(4, binding()));
    let leader = context.attestation_public();
    let _runtime = set_in_process_tee_context_for_test(Some(context));
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    {
        let mut guard = state.lock().expect("witness lock");
        guard.tx_index = Some(0);
        guard.is_user_candidate = true;
    }
    let _scope = WitnessExecutionScope::enter(Arc::clone(&state));
    let error = outbe_tee::consensus_call(
        "enclave_cause_seam",
        || {
            Err(TransportError::EnclaveCause {
                source: Box::new(Reason("deterministic refusal")),
            })
        },
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(matches!(error, TransportError::EnclaveCause { .. }));
    assert!(!error.is_node_local());
    assert_eq!(error.metric_class(), "enclave_error");
    let guard = state.lock().expect("witness lock");
    assert_eq!(guard.first_witness_fault, None);
    assert_eq!(
        guard.candidate_rejection,
        Some(CandidateRejection::UnsignedEnclaveError)
    );
}

fn assert_local_crypto_sentences() {
    use outbe_tee::TeeCryptoError as T;
    use LocalCryptoError as E;
    let plain = |expected: usize| T::InvalidPlaintextLength {
        expected,
        actual: 1,
    };
    for (error, expected) in [
        (
            E::OfferEncryptionRngFailure(T::RngFailure),
            "offer encryption rng failure",
        ),
        (E::HkdfExpand(T::HkdfExpand), "hkdf expand"),
        (E::HkdfFill(T::HkdfFill), "hkdf fill"),
        (E::AeadKey(T::AeadKey), "aead key"),
        (E::AeadSeal(T::AeadSeal), "aead seal"),
        (E::AeadOutputLength, "aead seal"),
        (
            E::InvalidFidelityCohortEnvelope,
            "invalid encrypted Fidelity cohort envelope",
        ),
        (
            E::FidelityCohortDecryptionFailed(T::DecryptionFailed),
            "Fidelity cohort decryption failed",
        ),
        (E::InvalidGratisBalance, "invalid encrypted Gratis balance"),
        (
            E::GratisBalanceDecryptionFailed(T::DecryptionFailed),
            "Gratis balance decryption failed",
        ),
        (
            E::InvalidGratisAmountLength(plain(32)),
            "invalid Gratis amount length",
        ),
        (E::InvalidPromisBalance, "invalid encrypted Promis balance"),
        (
            E::PromisBalanceDecryptionFailed(T::DecryptionFailed),
            "Promis balance decryption failed",
        ),
        (
            E::InvalidPromisAmountLength(plain(32)),
            "invalid Promis amount length",
        ),
        (E::InvalidNodEncoding, "invalid encrypted NOD encoding"),
        (E::InvalidNodAmountKey(T::AeadKey), "invalid NOD amount key"),
        (
            E::NodAmountDecryptionFailed(T::DecryptionFailed),
            "NOD amount decryption failed",
        ),
        (
            E::InvalidNodAmountLength(plain(32)),
            "invalid NOD amount length",
        ),
        (
            E::InvalidTributeEncoding,
            "invalid encrypted Tribute encoding",
        ),
        (
            E::InvalidTributeAmountKey(T::AeadKey),
            "invalid Tribute amount key",
        ),
        (
            E::TributeAmountDecryptionFailed(T::DecryptionFailed),
            "Tribute amount decryption failed",
        ),
        (
            E::InvalidTributeAmountLength(plain(64)),
            "invalid Tribute amount length",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
        assert_eq!(error, *expected);
        assert_ne!(error, *"other");
    }
}
