//! Session-only Promis account-key delivery.

use std::error::Error;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    chacha20poly1305_decrypt, chacha20poly1305_encrypt, derive_account_keys_message, eip191_hash,
    hkdf_sha256, BalanceTransition, Ledger, PROMIS,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_primitives::tee_signatures::recover_signer;
use outbe_promis::enclave_client::{derive_promis_keys, DerivePromisKeysRequest};
use outbe_promis::{
    derive_account_keys, derive_promis_keys_message, derive_view_key, modify_mac, ModifyOperation,
    PromisOp,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::resident::{
    AccountKeysHandle, OfferKeyHandle, ResidentOfferMaterial, StateKeyHandle,
};
use outbe_tee::witness::{
    EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
    WitnessState,
};
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::rand::SystemRandom;

const GROUP: &[u8] = b"promis-derive-group-signature-v1!!!!!!!!";
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

fn request(account: Address, recipient: [u8; 32], owner_sig: Vec<u8>) -> DerivePromisKeysRequest {
    DerivePromisKeysRequest {
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
fn promis_owner_signature_seals_account_keys_without_touching_the_tape() {
    let (_guard, context) = installed();
    let (_scope, tape) = witness_scope();
    let (private, recipient_pub) = recipient();
    let (account, signature) = owner_sig(Address::ZERO, Ledger::Promis, recipient_pub);
    assert_eq!(
        derive_promis_keys_message(account, B256::from(recipient_pub)),
        derive_account_keys_message(Ledger::Promis, account, B256::from(recipient_pub))
    );
    let sealed = derive_promis_keys(request(account, recipient_pub, signature)).expect("sealed");
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
        .state_key_handle(PROMIS.state_info(), 0)
        .expect("promis state key");
    let expected = derive_account_keys(&state_key, account).expect("account keys");
    let opened_keys = AccountKeysHandle::for_test(account, view, modify);
    let operation = ModifyOperation {
        account,
        op: PromisOp::Mint,
        amount: U256::from(1u64),
        op_nonce: 0,
        chain_id: chain_id(),
    };
    assert_eq!(
        modify_mac(&expected, &operation),
        modify_mac(&opened_keys, &operation)
    );
    let amount = U256::from(2_500_000u64);
    let blob = outbe_promis::cipher::write_amount(
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
    let opened_view = StateKeyHandle::for_test(view);
    let derived_view = derive_view_key(&state_key, account).expect("view key");
    assert_eq!(
        outbe_promis::decrypt_balance(&derived_view, account, &blob).expect("derived view"),
        outbe_promis::decrypt_balance(&opened_view, account, &blob).expect("opened view")
    );
    assert_tape_unchanged(&tape);
}

fn hex_bytes(text: &str) -> Vec<u8> {
    hex::decode(text).expect("hex")
}

fn hex_word(text: &str) -> [u8; 32] {
    hex_bytes(text).try_into().expect("32 bytes")
}

#[test]
fn historical_mcp_owner_proof_seals_promis_keys_and_rejects_a_mutated_signature() {
    const MESSAGE: &str = "6f757462652f70726f6d69732f6465726976652d6b6579732f763163467b02a7382408a845a5eb85b5238b8a4dd0edce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59";
    const SIGNATURE: &str = "0b20b5b12d996a78a3f0e5d3983e33a43754e534e4650843cfaf6beb164618ab00b87e14da6daa9fb5c70ebea37cdd2f758f5d85a23957915042f5d3dfaec5991b";
    let (_guard, context) = installed();
    let (_scope, tape) = witness_scope();
    let account: Address = "0x63467B02a7382408A845a5EB85b5238b8a4dD0eD"
        .parse()
        .expect("account");
    let ephemeral_secret =
        hex_word("0202020202020202020202020202020202020202020202020202020202020202");
    let ephemeral_pub =
        hex_word("ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59");
    let message = hex_bytes(MESSAGE);
    let signature = hex_bytes(SIGNATURE);
    assert_eq!(
        derive_promis_keys_message(account, B256::from(ephemeral_pub)),
        message
    );
    assert_eq!(
        derive_account_keys_message(Ledger::Promis, account, B256::from(ephemeral_pub)),
        message
    );
    let signature_bytes: [u8; 65] = signature.as_slice().try_into().expect("65-byte signature");
    assert_eq!(
        recover_signer(&eip191_hash(&message), &signature_bytes).expect("recover"),
        account
    );
    let sealed =
        derive_promis_keys(request(account, ephemeral_pub, signature.clone())).expect("sealed");
    assert_eq!(sealed.account, account);
    assert_eq!(sealed.sealed.len(), 80);
    let opened = OfferKeyHandle::for_test(ephemeral_secret)
        .ecdhe_decrypt(
            &ephemeral_pub,
            SHARE_INFO,
            &sealed.enclave_ephemeral_pubkey,
            &sealed.nonce,
            &sealed.sealed,
        )
        .expect("open historical share");
    assert_eq!(opened.len(), 64);
    let raw_view: [u8; 32] = opened[..32].try_into().expect("view");
    let state_key = context
        .state_key_handle(PROMIS.state_info(), 0)
        .expect("promis state key");
    let keys = derive_account_keys(&state_key, account).expect("account keys");
    let nonce = [1u8; 12];
    assert_eq!(
        keys.view_key_handle()
            .aead_encrypt(&nonce, b"client view")
            .expect("derived view"),
        chacha20poly1305_encrypt(&raw_view, &nonce, b"client view").expect("raw view")
    );
    assert_tape_unchanged(&tape);

    let mut mutated = signature;
    mutated[0] ^= 1;
    let rejected = derive_promis_keys(request(account, ephemeral_pub, mutated)).unwrap_err();
    assert!(!rejected.is_node_local(), "{rejected}");
    assert!(rejected.source().is_some(), "{rejected}");
    assert_tape_unchanged(&tape);
}

/// RFC 7748 X25519 on `p = 2^255 - 19`, 51-bit limbs. Test-only, not constant-time.
fn x25519(mut scalar: [u8; 32], point: [u8; 32]) -> [u8; 32] {
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    let x1 = fe_from_bytes(&point);
    let mut x2 = [1, 0, 0, 0, 0];
    let mut z2 = [0, 0, 0, 0, 0];
    let mut x3 = x1;
    let mut z3 = [1, 0, 0, 0, 0];
    let mut swap = 0u8;
    for t in (0..255).rev() {
        let kt = (scalar[t / 8] >> (t & 7)) & 1;
        swap ^= kt;
        fe_cswap(swap, &mut x2, &mut x3);
        fe_cswap(swap, &mut z2, &mut z3);
        swap = kt;
        let a = fe_add(x2, z2);
        let aa = fe_sq(a);
        let b = fe_sub(x2, z2);
        let bb = fe_sq(b);
        let e = fe_sub(aa, bb);
        let c = fe_add(x3, z3);
        let d = fe_sub(x3, z3);
        let da = fe_mul(d, a);
        let cb = fe_mul(c, b);
        x3 = fe_sq(fe_add(da, cb));
        z3 = fe_mul(x1, fe_sq(fe_sub(da, cb)));
        x2 = fe_mul(aa, bb);
        z2 = fe_mul(e, fe_add(aa, fe_mul_a24(e)));
    }
    fe_cswap(swap, &mut x2, &mut x3);
    fe_cswap(swap, &mut z2, &mut z3);
    fe_to_bytes(fe_mul(x2, fe_invert(z2)))
}

type Fe = [u64; 5];

fn fe_cswap(swap: u8, a: &mut Fe, b: &mut Fe) {
    if swap != 0 {
        std::mem::swap(a, b);
    }
}

fn fe_carry(mut t: [u128; 5]) -> Fe {
    let mask = (1u128 << 51) - 1;
    for _ in 0..4 {
        let mut c = 0u128;
        for limb in &mut t {
            let v = *limb + c;
            *limb = v & mask;
            c = v >> 51;
        }
        t[0] += c * 19;
    }
    let mut out = [0u64; 5];
    let mut c = 0u128;
    for i in 0..5 {
        let v = t[i] + c;
        out[i] = (v & mask) as u64;
        c = v >> 51;
    }
    let mut extra = c * 19;
    for limb in &mut out {
        let v = u128::from(*limb) + extra;
        *limb = (v & mask) as u64;
        extra = v >> 51;
    }
    out[0] = (u128::from(out[0]) + extra * 19) as u64;
    out
}

fn fe_add(a: Fe, b: Fe) -> Fe {
    fe_carry([
        u128::from(a[0]) + u128::from(b[0]),
        u128::from(a[1]) + u128::from(b[1]),
        u128::from(a[2]) + u128::from(b[2]),
        u128::from(a[3]) + u128::from(b[3]),
        u128::from(a[4]) + u128::from(b[4]),
    ])
}

fn fe_sub(a: Fe, b: Fe) -> Fe {
    // 2*p keeps every limb positive: 2*(2^51-19) = 2^52-38, 2*(2^51-1) = 2^52-2.
    let mut t = [0u128; 5];
    t[0] = u128::from(a[0]) + ((1u128 << 52) - 38) - u128::from(b[0]);
    for i in 1..5 {
        t[i] = u128::from(a[i]) + ((1u128 << 52) - 2) - u128::from(b[i]);
    }
    fe_carry(t)
}

fn fe_mul(a: Fe, b: Fe) -> Fe {
    let mut t = [0u128; 5];
    for (i, &ai) in a.iter().enumerate() {
        for (j, &bj) in b.iter().enumerate() {
            let product = u128::from(ai) * u128::from(bj);
            let k = i + j;
            if k < 5 {
                t[k] += product;
            } else {
                t[k - 5] += product * 19;
            }
        }
    }
    fe_carry(t)
}

fn fe_sq(a: Fe) -> Fe {
    fe_mul(a, a)
}

fn fe_mul_a24(e: Fe) -> Fe {
    fe_carry([
        u128::from(e[0]) * 121665,
        u128::from(e[1]) * 121665,
        u128::from(e[2]) * 121665,
        u128::from(e[3]) * 121665,
        u128::from(e[4]) * 121665,
    ])
}

fn fe_from_bytes(bytes: &[u8; 32]) -> Fe {
    let mut w = [0u64; 4];
    for i in 0..4 {
        w[i] = u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().expect("8 bytes"));
    }
    w[3] &= (1u64 << 63) - 1;
    let mask = (1u64 << 51) - 1;
    [
        w[0] & mask,
        ((w[0] >> 51) | (w[1] << 13)) & mask,
        ((w[1] >> 38) | (w[2] << 26)) & mask,
        ((w[2] >> 25) | (w[3] << 39)) & mask,
        (w[3] >> 12) & mask,
    ]
}

fn fe_canonical(f: Fe) -> Fe {
    let f = fe_carry([
        u128::from(f[0]),
        u128::from(f[1]),
        u128::from(f[2]),
        u128::from(f[3]),
        u128::from(f[4]),
    ]);
    let mask = (1u64 << 51) - 1;
    let p0 = mask - 18;
    if [f[1], f[2], f[3], f[4]] != [mask; 4] {
        return f;
    }
    match f[0].checked_sub(p0) {
        Some(low) => [low, 0, 0, 0, 0],
        None => f,
    }
}

fn fe_to_bytes(f: Fe) -> [u8; 32] {
    let f = fe_canonical(f);
    let w = [
        f[0] | (f[1] << 51),
        (f[1] >> 13) | (f[2] << 38),
        (f[2] >> 26) | (f[3] << 25),
        (f[3] >> 39) | (f[4] << 12),
    ];
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[8 * i..8 * i + 8].copy_from_slice(&w[i].to_le_bytes());
    }
    out
}

fn fe_invert(z: Fe) -> Fe {
    let mut r = [1, 0, 0, 0, 0];
    for t in (0..255).rev() {
        r = fe_sq(r);
        let byte = if t < 8 {
            0xeb
        } else if t >= 248 {
            0x7f
        } else {
            0xff
        };
        if (byte >> (t & 7)) & 1 == 1 {
            r = fe_mul(r, z);
        }
    }
    r
}

#[test]
fn historical_mcp_sealed_keys_match_the_frozen_share() {
    let recipient_pub =
        hex_word("ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59");
    let enclave_pub = OfferKeyHandle::for_test([0x01; 32]).public();
    assert_eq!(
        enclave_pub,
        hex_word("a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209")
    );
    let shared = x25519([0x01; 32], recipient_pub);
    let key = hkdf_sha256(&recipient_pub, &shared, SHARE_INFO).expect("share key");
    let nonce_okm =
        hkdf_sha256(&recipient_pub, &shared, b"outbe/tee/registry-seal-nonce/v1").expect("nonce");
    let nonce: [u8; 12] = nonce_okm[..12].try_into().expect("12-byte nonce");
    let mut plaintext = [0u8; 64];
    plaintext[..32].fill(0x5a);
    plaintext[32..].fill(0x6b);
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, &plaintext).expect("seal");
    assert_eq!(
        nonce,
        <[u8; 12]>::try_from(hex_bytes("ab02a2e3c93cbf650bbcb1e5").as_slice()).expect("nonce")
    );
    assert_eq!(
        ciphertext,
        hex_bytes("80a9b32b46568b2ea28136735fa781f6b793f6bbfcb79229b25819b263cc0a00bbc312b2adc6c8993e00454d31a0e22016eb63d08f4116e4160c86db327e0ba2e3cec9b2ebb2720d0e5f3fa104c71c4e")
    );
    let opened = OfferKeyHandle::for_test([0x02; 32])
        .ecdhe_decrypt(
            &recipient_pub,
            SHARE_INFO,
            &enclave_pub,
            &nonce,
            &ciphertext,
        )
        .expect("open");
    assert_eq!(&*opened, &plaintext);
}

#[test]
fn foreign_owner_proofs_keep_their_cause_and_a_missing_runtime_is_unavailable() {
    let (_guard, _context) = installed();
    let (_scope, tape) = witness_scope();
    let (_, recipient_pub) = recipient();
    let (account, promis_sig) = owner_sig(Address::ZERO, Ledger::Promis, recipient_pub);
    let (_, gratis_sig) = owner_sig(account, Ledger::Gratis, recipient_pub);
    let foreign_ledger =
        derive_promis_keys(request(account, recipient_pub, gratis_sig)).unwrap_err();
    assert_owner_rejection(
        foreign_ledger,
        "owner signature does not control this account",
    );
    assert_tape_unchanged(&tape);

    let mut other_recipient = recipient_pub;
    other_recipient[0] ^= 1;
    let wrong_recipient =
        derive_promis_keys(request(account, other_recipient, promis_sig)).unwrap_err();
    assert_owner_rejection(
        wrong_recipient,
        "owner signature does not control this account",
    );
    assert_tape_unchanged(&tape);

    let short = derive_promis_keys(request(account, recipient_pub, vec![0x11, 0x22])).unwrap_err();
    assert_owner_rejection(short, "owner signature must be 65 bytes");
    assert_tape_unchanged(&tape);

    drop(_guard);
    let _absent = set_in_process_tee_context_for_test(None);
    let (account, signature) = owner_sig(Address::ZERO, Ledger::Promis, recipient_pub);
    let missing = derive_promis_keys(request(account, recipient_pub, signature)).unwrap_err();
    assert!(
        matches!(missing, PrecompileError::EnclaveUnavailable(ref message) if message == "tee_sidecar_unavailable: outbe_derivePromisKeys"),
        "{missing}"
    );
    assert_tape_unchanged(&tape);
}
