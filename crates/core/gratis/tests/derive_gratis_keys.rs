//! Session-only Gratis account-key delivery.

use std::error::Error;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    chacha20poly1305_decrypt, derive_account_keys_message, eip191_hash, hkdf_sha256,
    BalanceTransition, Ledger, GRATIS,
};
use outbe_gratis::enclave_client::{derive_gratis_keys, DeriveGratisKeysRequest};
use outbe_gratis::{
    derive_account_keys, derive_gratis_keys_message, modify_mac, GratisOp, ModifyOperation,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::resident::{AccountKeysHandle, ResidentOfferMaterial, StateKeyHandle};
use outbe_tee::witness::{
    EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
    WitnessState,
};
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::rand::SystemRandom;

const GROUP: &[u8] = b"gratis-derive-group-signature-v1!!!!!!!!";
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

fn installed() -> (
    outbe_tee::in_process_dispatch::InProcessTestGuard,
    Arc<InProcessTeeContext>,
) {
    let context = Arc::new(outbe_tee::test_utils::resident_context(4, binding()));
    let offer = ResidentOfferMaterial::for_test(GROUP.to_vec(), chain_id(), 0, 0).expect("offer");
    context.activate_for_test(offer).expect("activate");
    let guard = set_in_process_tee_context_for_test(Some(Arc::clone(&context)));
    (guard, context)
}

fn witness_scope() -> (WitnessExecutionScope, Arc<Mutex<WitnessState>>) {
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some([0x44; 32]),
        EnclaveWitnessTapeV1::default(),
    )));
    state.lock().expect("witness lock").prefix_cursor = 7;
    state.lock().expect("witness lock").staged.push(marker());
    let scope = WitnessExecutionScope::enter(Arc::clone(&state));
    (scope, state)
}

fn marker() -> EnclaveWitnessEntryV1 {
    EnclaveWitnessEntryV1::Payload {
        payload: vec![0x11],
        inputs_canonical_hash: B256::repeat_byte(0x22),
        attestation_tag: vec![0x33],
    }
}

fn assert_tape_unchanged(state: &Mutex<WitnessState>) {
    let witness = state.lock().expect("witness lock");
    assert_eq!(witness.prefix_cursor, 7);
    assert_eq!(witness.staged, vec![marker()]);
}

fn recipient() -> (EphemeralPrivateKey, [u8; 32]) {
    let private = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new()).expect("x25519");
    let public = private.compute_public_key().expect("public");
    let bytes: [u8; 32] = public
        .as_ref()
        .try_into()
        .expect("x25519 public key is 32 bytes");
    (private, bytes)
}

fn owner_sig(account: Address, ledger: Ledger, recipient: [u8; 32]) -> (Address, Vec<u8>) {
    let signer = OutbeEvmSigner::from_secret_bytes([7; 32]).expect("owner key");
    let account = if account == Address::ZERO {
        signer.address()
    } else {
        account
    };
    let message = derive_account_keys_message(ledger, account, B256::from(recipient));
    let signature = signer
        .sign_hash(&eip191_hash(&message))
        .expect("owner signature");
    (account, signature.to_vec())
}

fn request(account: Address, recipient: [u8; 32], owner_sig: Vec<u8>) -> DeriveGratisKeysRequest {
    DeriveGratisKeysRequest {
        account,
        requester_ephemeral_pubkey: recipient,
        owner_sig,
    }
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
    .expect("share");
    let key = hkdf_sha256(recipient, &shared, SHARE_INFO).expect("hkdf");
    chacha20poly1305_decrypt(&key, nonce, ciphertext).expect("open")
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
fn gratis_owner_signature_seals_account_keys_without_touching_the_tape() {
    let (_guard, context) = installed();
    let (_scope, tape) = witness_scope();
    let (private, recipient_pub) = recipient();
    let (account, signature) = owner_sig(Address::ZERO, Ledger::Gratis, recipient_pub);
    assert_eq!(
        derive_gratis_keys_message(account, B256::from(recipient_pub)),
        derive_account_keys_message(Ledger::Gratis, account, B256::from(recipient_pub))
    );
    let sealed = derive_gratis_keys(request(account, recipient_pub, signature)).expect("sealed");
    assert_eq!(sealed.account, account);
    assert_eq!(sealed.sealed.len(), 80);
    let opened = open_share(
        private,
        &recipient_pub,
        &sealed.enclave_ephemeral_pubkey,
        &sealed.nonce,
        &sealed.sealed,
    );
    let view: [u8; 32] = opened[..32].try_into().expect("view");
    let modify: [u8; 32] = opened[32..].try_into().expect("modify");
    let state_key = context
        .state_key_handle(GRATIS.state_info(), 0)
        .expect("gratis state key");
    let expected = derive_account_keys(&state_key, account).expect("account keys");
    let opened_keys = AccountKeysHandle::for_test(account, view, modify);
    let operation = ModifyOperation {
        account,
        op: GratisOp::Mint,
        amount: U256::from(1u64),
        op_nonce: 0,
        chain_id: chain_id(),
    };
    assert_eq!(
        modify_mac(&expected, &operation),
        modify_mac(&opened_keys, &operation)
    );
    let amount = U256::from(2_500_000u64);
    let blob = outbe_gratis::cipher::write_amount(
        expected.view_key_handle(),
        BalanceTransition {
            account,
            field: 0,
            previous_version: 1,
            amount,
            input_hash: B256::repeat_byte(0x04),
        },
    )
    .expect("seal balance");
    assert_eq!(
        outbe_gratis::decrypt_balance(&StateKeyHandle::for_test(view), account, &blob)
            .expect("view"),
        amount
    );
    assert_tape_unchanged(&tape);
}

#[test]
fn foreign_owner_proofs_keep_their_cause_and_a_missing_runtime_is_unavailable() {
    let (_guard, _context) = installed();
    let (_scope, tape) = witness_scope();
    let (_, recipient_pub) = recipient();
    let (account, gratis_sig) = owner_sig(Address::ZERO, Ledger::Gratis, recipient_pub);
    let (_, promis_sig) = owner_sig(account, Ledger::Promis, recipient_pub);
    let promis = derive_gratis_keys(request(account, recipient_pub, promis_sig)).unwrap_err();
    assert_owner_rejection(promis, "owner signature does not control this account");
    assert_tape_unchanged(&tape);

    let mut other_recipient = recipient_pub;
    other_recipient[0] ^= 1;
    let wrong_recipient =
        derive_gratis_keys(request(account, other_recipient, gratis_sig)).unwrap_err();
    assert_owner_rejection(
        wrong_recipient,
        "owner signature does not control this account",
    );
    assert_tape_unchanged(&tape);

    let short = derive_gratis_keys(request(account, recipient_pub, vec![0x11, 0x22])).unwrap_err();
    assert_owner_rejection(short, "owner signature must be 65 bytes");
    assert_tape_unchanged(&tape);

    drop(_guard);
    let _absent = set_in_process_tee_context_for_test(None);
    let (account, signature) = owner_sig(Address::ZERO, Ledger::Gratis, recipient_pub);
    let missing = derive_gratis_keys(request(account, recipient_pub, signature)).unwrap_err();
    assert!(
        matches!(missing, PrecompileError::EnclaveUnavailable(ref message) if message == "tee_sidecar_unavailable: outbe_deriveGratisKeys"),
        "{missing}"
    );
    assert_tape_unchanged(&tape);
}
