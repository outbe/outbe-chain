//! Consensus witness modes and the session derive / canary calls.

use std::error::Error;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    canary_offer_json, chacha20poly1305_decrypt, derive_account_keys_message, eip191_hash,
    encrypt_tribute_offer, hkdf_sha256, Ledger, TRIBUTE,
};
use outbe_core_common::tribute_encryption::{TributeAmountsV2, TributeContextV2};
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_system_common::time::WorldwideDay;
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::resident::{ResidentOfferMaterial, StateKeyHandle};
use outbe_tee::witness::{
    BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, TxWitnessV1,
    WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};
use outbe_tribute::confidential::encrypt_tribute;
use outbe_tribute::confidential::host::{
    decrypt_canary_tribute_offer, derive_tribute_keys, open_canary_tribute_offer,
    read_tribute_amounts, DeriveTributeKeysRequest,
};
use outbe_tribute::confidential::EncryptedTributeOffer;
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::rand::SystemRandom;

const GROUP: &[u8] = b"tribute-witness-group-signature-v1!!!!";
const SHARE_INFO: &[u8] = b"outbe/tee/dkg-share/v1";

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

fn activated(seed: u8) -> Arc<InProcessTeeContext> {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    let offer = ResidentOfferMaterial::for_test(GROUP.to_vec(), chain_id(), 0, 0).unwrap();
    context.activate_for_test(offer).unwrap();
    context
}

fn install(
    context: &Arc<InProcessTeeContext>,
) -> outbe_tee::in_process_dispatch::InProcessTestGuard {
    set_in_process_tee_context_for_test(Some(Arc::clone(context)))
}

fn enter(
    mode: WitnessExecutionMode,
    leader: [u8; 32],
    tape: Vec<EnclaveWitnessEntryV1>,
) -> (Arc<Mutex<WitnessState>>, WitnessExecutionScope) {
    let state = Arc::new(Mutex::new(WitnessState::new(
        mode,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: tape,
            txs: Vec::new(),
        },
    )));
    let scope = WitnessExecutionScope::enter(Arc::clone(&state));
    (state, scope)
}

fn sample(
    context: &InProcessTeeContext,
) -> outbe_core_common::tribute_encryption::EncryptedTributeV2 {
    let key = context.state_key_handle(TRIBUTE.state_info(), 0).unwrap();
    let day = WorldwideDay::new(20250115);
    encrypt_tribute(
        &key,
        TributeContextV2 {
            chain_id: 1,
            tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(0x22)),
            owner: Address::repeat_byte(0x33),
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 978,
            tribute_price_minor: U256::from(2_000_000),
            offer_input_hash: B256::repeat_byte(0x44),
        },
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(100),
            nominal_amount_minor: U256::from(50),
        },
    )
    .unwrap()
}

fn record(
    context: &Arc<InProcessTeeContext>,
    tribute: &outbe_core_common::tribute_encryption::EncryptedTributeV2,
) -> Vec<EnclaveWitnessEntryV1> {
    let _guard = install(context);
    let (state, scope) = enter(
        WitnessExecutionMode::ProposerRecord,
        context.attestation_public(),
        Vec::new(),
    );
    read_tribute_amounts(vec![tribute.clone()]).unwrap();
    let staged = state.lock().unwrap().staged.clone();
    drop(scope);
    staged
}

fn payload_mut(entry: &mut EnclaveWitnessEntryV1) -> &mut Vec<u8> {
    let EnclaveWitnessEntryV1::Payload { payload, .. } = entry else {
        panic!("recorded entry is not Payload");
    };
    payload
}

fn marker() -> EnclaveWitnessEntryV1 {
    EnclaveWitnessEntryV1::Payload {
        payload: vec![0x11],
        inputs_canonical_hash: B256::repeat_byte(0x22),
        attestation_tag: vec![0x33],
    }
}

fn session_scope() -> (WitnessExecutionScope, Arc<Mutex<WitnessState>>) {
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some([0x44; 32]),
        EnclaveWitnessTapeV1::default(),
    )));
    state.lock().unwrap().prefix_cursor = 7;
    state.lock().unwrap().staged.push(marker());
    let scope = WitnessExecutionScope::enter(Arc::clone(&state));
    (scope, state)
}

fn assert_tape_unchanged(state: &Mutex<WitnessState>) {
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 7);
    assert_eq!(witness.staged, vec![marker()]);
}

fn cause_chain(error: &dyn Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut current = error.source();
    while let Some(source) = current {
        parts.push(source.to_string());
        current = source.source();
    }
    parts.join(" | ")
}

fn assert_owner_rejection(error: PrecompileError, cause: &str) {
    assert!(!error.is_node_local(), "{error}");
    let chain = cause_chain(&error);
    assert!(error.source().is_some(), "{chain}");
    assert!(chain.contains(cause), "{chain}");
}

#[test]
fn not_ready_read_is_local_unavailable() {
    let context = Arc::new(outbe_tee::test_utils::resident_context(4, binding()));
    let _guard = install(&context);
    let (state, _scope) = enter(
        WitnessExecutionMode::ProposerRecord,
        context.attestation_public(),
        Vec::new(),
    );
    let error = read_tribute_amounts(Vec::new()).unwrap_err();
    assert!(error.is_node_local(), "{error}");
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 0);
    assert!(witness.staged.is_empty());
    assert_eq!(
        witness.first_witness_fault,
        Some(BlockWitnessFault::LocalUnavailable)
    );
}

#[test]
fn replay_of_a_different_request_is_tape_invalid_at_cursor_zero() {
    let context = activated(4);
    let tribute = sample(&context);
    let leader = context.attestation_public();
    let tape = record(&context, &tribute);
    let mut other = tribute;
    other.encrypted_amounts[8] ^= 1;
    let _absent = set_in_process_tee_context_for_test(None);
    let (state, _scope) = enter(WitnessExecutionMode::VerifierReplay, leader, tape);
    assert!(read_tribute_amounts(vec![other]).is_err());
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(
        witness.first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
}

#[test]
fn wrong_tag_on_the_typed_tape_is_tape_invalid() {
    let context = activated(4);
    let tribute = sample(&context);
    let leader = context.attestation_public();
    let mut tape = record(&context, &tribute);
    payload_mut(&mut tape[0])[0] = 14;
    let _absent = set_in_process_tee_context_for_test(None);
    let (state, _scope) = enter(WitnessExecutionMode::VerifierReplay, leader, tape);
    assert!(read_tribute_amounts(vec![tribute]).is_err());
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(
        witness.first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
}

#[test]
fn tx_index_replay_consumes_the_matching_body_entry() {
    let context = activated(4);
    let tribute = sample(&context);
    let leader = context.attestation_public();
    let entries = record(&context, &tribute);
    let _absent = set_in_process_tee_context_for_test(None);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: Vec::new(),
            txs: vec![TxWitnessV1 {
                tx_index: 0,
                entries,
            }],
        },
    )));
    state.lock().unwrap().tx_index = Some(0);
    let _scope = WitnessExecutionScope::enter(Arc::clone(&state));
    read_tribute_amounts(vec![tribute]).unwrap();
    let witness = state.lock().unwrap();
    assert!(witness.first_witness_fault.is_none());
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(witness.tx_cursor, 0);
    assert_eq!(witness.replay_cursor, 1);
}

#[test]
fn verifier_with_enclave_accepts_a_different_attestation_key() {
    let leader_ctx = activated(4);
    let tribute = sample(&leader_ctx);
    let leader = leader_ctx.attestation_public();
    let tape = record(&leader_ctx, &tribute);
    let local = activated(9);
    assert_ne!(local.attestation_public(), leader);
    let _guard = install(&local);
    let (state, _scope) = enter(WitnessExecutionMode::VerifierWithEnclave, leader, tape);
    read_tribute_amounts(vec![tribute]).unwrap();
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 1);
    assert!(witness.first_witness_fault.is_none());
}

#[test]
fn rpc_direct_executes_without_staging() {
    let context = activated(4);
    let tribute = sample(&context);
    let _guard = install(&context);
    let (state, _scope) = enter(
        WitnessExecutionMode::RpcDirect,
        context.attestation_public(),
        Vec::new(),
    );
    read_tribute_amounts(vec![tribute]).unwrap();
    let witness = state.lock().unwrap();
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 0);
    assert!(witness.first_witness_fault.is_none());
}

fn recipient() -> (EphemeralPrivateKey, [u8; 32]) {
    let private = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new()).unwrap();
    let public = private.compute_public_key().unwrap();
    let bytes: [u8; 32] = public.as_ref().try_into().unwrap();
    (private, bytes)
}

fn owner_sig(ledger: Ledger, recipient: [u8; 32]) -> (Address, Vec<u8>) {
    let signer = OutbeEvmSigner::from_secret_bytes([7; 32]).unwrap();
    let account = signer.address();
    let message = derive_account_keys_message(ledger, account, B256::from(recipient));
    let signature = signer.sign_hash(&eip191_hash(&message)).unwrap();
    (account, signature.to_vec())
}

fn open_share(
    private: EphemeralPrivateKey,
    recipient: &[u8; 32],
    enclave_pub: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Vec<u8> {
    let peer = UnparsedPublicKey::new(&X25519, enclave_pub);
    let shared = agree_ephemeral(private, &peer, |shared| {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(shared);
        bytes
    })
    .unwrap();
    let key = hkdf_sha256(recipient, &shared, SHARE_INFO).unwrap();
    chacha20poly1305_decrypt(&key, nonce, ciphertext).unwrap()
}

#[test]
fn derive_tribute_keys_seals_the_owner_and_leaves_the_tape() {
    let context = activated(4);
    let _guard = install(&context);
    let (_scope, tape) = session_scope();
    let (private, recipient_pub) = recipient();
    let (account, signature) = owner_sig(Ledger::Tribute, recipient_pub);
    let sealed = derive_tribute_keys(DeriveTributeKeysRequest {
        account,
        requester_ephemeral_pubkey: recipient_pub,
        owner_sig: signature,
    })
    .unwrap();
    assert_eq!(sealed.account, account);
    let opened = open_share(
        private,
        &recipient_pub,
        &sealed.enclave_ephemeral_pubkey,
        &sealed.nonce,
        &sealed.sealed,
    );
    let view: [u8; 32] = opened[..32].try_into().unwrap();
    let state_key = context.state_key_handle(TRIBUTE.state_info(), 0).unwrap();
    let expected = TRIBUTE
        .account_keys
        .derive_account_keys(&state_key, account)
        .unwrap();
    let nonce = [1; 12];
    assert_eq!(
        expected
            .view_key_handle()
            .aead_encrypt(&nonce, b"client view")
            .unwrap(),
        StateKeyHandle::for_test(view)
            .aead_encrypt(&nonce, b"client view")
            .unwrap()
    );
    assert_tape_unchanged(&tape);
}

#[test]
fn foreign_owner_and_a_missing_runtime_keep_their_classes() {
    let context = activated(4);
    let _guard = install(&context);
    let (_scope, tape) = session_scope();
    let (_, recipient_pub) = recipient();
    let (account, _) = owner_sig(Ledger::Tribute, recipient_pub);
    let (_, promis_sig) = owner_sig(Ledger::Promis, recipient_pub);
    let promis = derive_tribute_keys(DeriveTributeKeysRequest {
        account,
        requester_ephemeral_pubkey: recipient_pub,
        owner_sig: promis_sig,
    })
    .unwrap_err();
    assert_owner_rejection(promis, "owner signature does not control this account");
    let short = derive_tribute_keys(DeriveTributeKeysRequest {
        account,
        requester_ephemeral_pubkey: recipient_pub,
        owner_sig: vec![0x11, 0x22],
    })
    .unwrap_err();
    assert_owner_rejection(short, "owner signature must be 65 bytes");
    assert_tape_unchanged(&tape);
    drop(_guard);
    let _absent = set_in_process_tee_context_for_test(None);
    let (account, signature) = owner_sig(Ledger::Tribute, recipient_pub);
    let missing = derive_tribute_keys(DeriveTributeKeysRequest {
        account,
        requester_ephemeral_pubkey: recipient_pub,
        owner_sig: signature,
    })
    .unwrap_err();
    assert!(missing.is_node_local(), "{missing}");
    assert!(
        matches!(missing, PrecompileError::EnclaveUnavailable(ref text) if text.contains("not installed")),
        "{missing}"
    );
    assert_tape_unchanged(&tape);
}

fn canary_offer(context: &InProcessTeeContext) -> EncryptedTributeOffer {
    let offer_pub = context
        .with_offer_key_handle(|handle| handle.public())
        .unwrap();
    let plaintext = canary_offer_json(20250115);
    let (cipher_text, nonce, ephemeral) =
        encrypt_tribute_offer(&offer_pub, plaintext.as_bytes()).unwrap();
    EncryptedTributeOffer {
        owner: Address::repeat_byte(1),
        cipher_text,
        nonce: nonce.to_vec(),
        ephemeral_pubkey: U256::from_be_bytes(ephemeral),
        worldwide_day: WorldwideDay::new(20250115),
        tribute_currency: 999,
        reference_currency: 840,
        issuance_wwd_vwap_minor: U256::ZERO,
        reference_wwd_vwap_minor: U256::ZERO,
        reference_scurve_minor: U256::ZERO,
        zk_context: None,
    }
}

#[test]
fn canary_decrypt_matches_the_offer_plaintext_and_leaves_the_tape() {
    let context = activated(4);
    let offer = canary_offer(&context);
    let opened = open_canary_tribute_offer(&context, &offer).unwrap();
    assert_eq!(opened, canary_offer_json(20250115).into_bytes());
    let _guard = install(&context);
    let (_scope, tape) = session_scope();
    let decrypted = decrypt_canary_tribute_offer(offer).unwrap();
    assert_eq!(decrypted, opened);
    assert_tape_unchanged(&tape);
}
