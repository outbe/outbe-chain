//! Pure NOD ciphers, canonical preimages, and witness replay for tags 10, 19, 20, and 21.
use std::sync::{Arc, Mutex};

use alloy_primitives::{hex, keccak256, Address, B256, U256};
use outbe_core_common::confidential_tee::{
    attestation, derive_resident_state_key, hkdf_sha256, offer_material, sign_attestation_tag,
    NOD as NOD_DOMAIN,
};
use outbe_core_common::nod_encryption::{
    EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1, NodTermsV1,
};
use outbe_nod::nod_decrypt::decrypt_nod_for_owner;
use outbe_nod::nod_encryption::{
    apply_nod_group_load_operation as apply_pure, decrypt_nod, encrypt_nod, encrypt_nod_quote,
    nod_group_load_attestation_preimage, nod_group_load_inputs_hash, nod_group_load_read_hash,
    nod_group_load_read_preimage, nod_quote_inputs_hash, nod_read_hash, nod_read_preimage,
    read_nod_group_load as read_load_pure, seal_group_load, NodGroupLoadOpRequestV1,
    NodGroupLoadOperationV1,
};
use outbe_nod::nod_host::{
    apply_nod_group_load_operation, encrypt_nod_quote as encrypt_quote_host, read_nod_amount,
    read_nod_group_load, sign_nod_host_for_test, NodAmountReadV1, NodGroupLoadOpAppliedV1,
    NodGroupLoadReadV1, NodQuoteEncryptedV1, SignedNodHost,
};
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_tee::call_context::current_witness_state;
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::in_process_dispatch::{
    in_process_tee_context, replace_in_process_tee_context, set_in_process_tee_context_for_test,
};
use outbe_tee::resident::StateKeyHandle;
use outbe_tee::witness::{
    BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode,
    WitnessExecutionScope, WitnessState,
};
use outbe_tee::InProcessDispatchError;
use outbe_tee::TransportError;

const TERMS: &str = r#"{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"league_id":7,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978}"#;
const NOD_FIXTURE: &str = r#"{"terms":{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"league_id":7,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978},"encryption_binding":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_gratis_amount":[65]}"#;
const LOAD: &str = r#"{"chain_id":1,"scope":"0x2222222222222222222222222222222222222222222222222222222222222222","operation_hash":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_load":[65]}"#;
const LOAD_REQUEST: &str = r#"{"chain_id":1,"scope":"0x2222222222222222222222222222222222222222222222222222222222222222","previous":{"chain_id":1,"scope":"0x2222222222222222222222222222222222222222222222222222222222222222","operation_hash":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_load":[65]},"operation":{"AdjustTransient":{"load_minor":"0x11","add":false}}}"#;
const QUOTE_CONTEXT: &str = r#"{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","asset":"0x4444444444444444444444444444444444444444","settlement_currency":840,"snapshot_id":"0x5"}"#;
const AMOUNT_READ_SIGNED: &str = "0a200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152";
const GROUP_LOAD_SIGNED: &str = "13012022222222222222222222222222222222222222222222222222222222222222222033333333333333333333333333333333333333333333333333333333333333330141201111111111111111111111111111111111111111111111111111111111111111025152";
const GROUP_READ_SIGNED: &str = "14200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152";
const QUOTE_SIGNED: &str =
    "15024142201111111111111111111111111111111111111111111111111111111111111111025152";

fn parse<T: serde::de::DeserializeOwned>(json: &str) -> T {
    serde_json::from_str(json).unwrap()
}

fn fixture_key() -> StateKeyHandle {
    StateKeyHandle::for_test([0x11; 32])
}

fn tag_free(signed_hex: &str) -> Vec<u8> {
    let bytes = hex::decode(signed_hex).unwrap();
    let at = bytes
        .windows(3)
        .position(|window| window == [2, 0x51, 0x52])
        .unwrap();
    assert_eq!(
        bytes
            .windows(3)
            .filter(|window| *window == [2, 0x51, 0x52])
            .count(),
        1
    );
    let mut payload = Vec::with_capacity(bytes.len() - 2);
    payload.extend_from_slice(&bytes[..at]);
    payload.push(0);
    payload.extend_from_slice(&bytes[at + 3..]);
    payload
}

fn assert_tag_free_body(tag: u8, body: &impl serde::Serialize, signed_hex: &str) {
    let mut payload = vec![tag];
    payload.extend(postcard::to_allocvec(body).unwrap());
    assert_eq!(payload, tag_free(signed_hex));
}

#[test]
fn cipher_and_preimage_vectors_match_the_frozen_nod_answers() {
    let terms: NodTermsV1 = parse(TERMS);
    let nod = encrypt_nod(&fixture_key(), terms, U256::from(17), U256::from(17)).unwrap();
    assert_eq!(
        hex::encode(nod.encryption_binding),
        "d2f75a55db07c33cccf750c02012e077beaaf5ee33877c53943be451c3db6bd9"
    );
    assert_eq!(
        hex::encode(&nod.encrypted_gratis_amount),
        "0000000000000001669f99a7b9d70b9cb0cd668126a4c34d44acf8f56b90b94c9b8d093a5ba1e066e3d903c2c91c3a479dfa81082b9c147a"
    );
    let sealed = seal_group_load(
        &fixture_key(),
        (1, B256::repeat_byte(0x22)),
        (4, B256::repeat_byte(0x33)),
        U256::from(17),
    )
    .unwrap();
    assert_eq!(
        hex::encode(sealed.operation_hash),
        "19a504024cb859540b7e5f16c23ea58e63a9a686b7b3d4fc77aadebfce515728"
    );
    assert_eq!(
        hex::encode(&sealed.encrypted_load),
        "00000000000000050df785dc512c51a800f6a87718dc2b42527e8506f38b3a58d346adac0043beac9ecc66369afc09a12d5b6fe3c170a337"
    );
    assert_eq!(
        read_load_pure(&fixture_key(), &sealed).unwrap(),
        U256::from(17)
    );
    let context: NodQuoteContextV1 = parse(QUOTE_CONTEXT);
    let quote = encrypt_nod_quote(&fixture_key(), &nod, &context, U256::from(18)).unwrap();
    assert_eq!(
        hex::encode(quote),
        "000000000000000182cfc1644fef4b01724c9f47659e77518492bdf99def8013e2774ec4e97da20695839161d5f79728ab26f93db62c2cf1"
    );

    let inputs = B256::repeat_byte(0x11);
    let fixture_nod: EncryptedNodV1 = parse(NOD_FIXTURE);
    let load: EncryptedNodGroupLoadV1 = parse(LOAD);
    let request: NodGroupLoadOpRequestV1 = parse(LOAD_REQUEST);
    assert_eq!(
        hex::encode(nod_read_hash(&fixture_nod).unwrap()),
        "ac7fe9f255dcda9ad3ce660a537058a7714eba59ef4447e3da4ea07f0a50af25"
    );
    assert_eq!(
        hex::encode(keccak256(nod_read_preimage(inputs, U256::from(17)))),
        "9460df2fedef56a57d91681c662d511e12537af23d09db9f44d31e5a3ea7ba9d"
    );
    assert_eq!(
        hex::encode(nod_group_load_inputs_hash(&request).unwrap()),
        "629681c2ce3d153ca94f850dd3e820e1f2a051eda3aa2f1ffe5bdee95e2cc499"
    );
    assert_eq!(
        hex::encode(keccak256(
            nod_group_load_attestation_preimage(inputs, &load).unwrap()
        )),
        "91df1d4253a46b88e14e87bf7bc77333ff2c3dae42a2b50f7e1030355ad21f5c"
    );
    assert_eq!(
        hex::encode(nod_group_load_read_hash(&load).unwrap()),
        "3024c13b2ff8b344bdb01060d776d753c983fd4dbf3886edd72463755d320e55"
    );
    assert_eq!(
        hex::encode(keccak256(nod_group_load_read_preimage(
            inputs,
            U256::from(17)
        ))),
        "f6911971f7f3a279f95d3fc0545722bf14c68c4c8614c693b825f2ace34b2cce"
    );
    let fixture_context: NodQuoteContextV1 = parse(QUOTE_CONTEXT);
    assert_eq!(
        hex::encode(nod_quote_inputs_hash(&fixture_nod, &fixture_context, U256::from(18)).unwrap()),
        "b665b0b89642a4117ce7ae1b6565d2d33e1d3462046879c722c0b05188ca4263"
    );
}

#[test]
fn tag_free_postcard_bodies_match_the_signed_response_vectors() {
    let hash = B256::repeat_byte(0x11);
    let load: EncryptedNodGroupLoadV1 = parse(LOAD);
    assert_tag_free_body(
        10,
        &NodAmountReadV1 {
            amount: U256::from(0x11u64),
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
        },
        AMOUNT_READ_SIGNED,
    );
    assert_tag_free_body(
        19,
        &NodGroupLoadOpAppliedV1 {
            record: load,
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
        },
        GROUP_LOAD_SIGNED,
    );
    assert_tag_free_body(
        20,
        &NodGroupLoadReadV1 {
            amount: U256::from(0x11u64),
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
        },
        GROUP_READ_SIGNED,
    );
    assert_tag_free_body(
        21,
        &NodQuoteEncryptedV1 {
            encrypted_payment: vec![0x41, 0x42],
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
        },
        QUOTE_SIGNED,
    );
}

#[test]
fn same_amount_encrypts_equal_and_owner_open_rejects_tampering() {
    let nod_key = StateKeyHandle::for_test([8u8; 32]);
    let owner = Address::repeat_byte(3);
    let view = hkdf_sha256(&[8u8; 32], owner.as_slice(), b"outbe/nod/view-key/v1").unwrap();
    let terms = NodTermsV1 {
        chain_id: 1,
        nod_id: U256::from(2u64),
        owner,
        emission_day: 20261116,
        entry_price_minor: U256::from(3u64),
        issuance_currency: 840,
        reference_currency: 978,
    };
    let first = encrypt_nod(&nod_key, terms.clone(), U256::from(123), U256::from(123)).unwrap();
    let repeated = encrypt_nod(&nod_key, terms.clone(), U256::from(123), U256::from(123)).unwrap();
    let divergent = encrypt_nod(&nod_key, terms, U256::from(124), U256::from(124)).unwrap();
    assert_eq!(first, repeated);
    assert_ne!(first.encryption_binding, divergent.encryption_binding);
    assert_ne!(
        first.encrypted_gratis_amount,
        divergent.encrypted_gratis_amount
    );
    assert_eq!(decrypt_nod(&nod_key, &first).unwrap(), U256::from(123));
    assert_eq!(decrypt_nod(&nod_key, &first).unwrap(), U256::from(123));
    assert_eq!(
        decrypt_nod_for_owner(&view, &first).unwrap(),
        U256::from(123)
    );
    assert!(decrypt_nod_for_owner(&[12u8; 32], &first).is_err());
    assert!(decrypt_nod(&StateKeyHandle::for_test([9u8; 32]), &first).is_err());
    let mut tampered = first.clone();
    tampered.terms.owner = Address::repeat_byte(9);
    assert!(decrypt_nod(&nod_key, &tampered).is_err());
    let mut tampered = first;
    tampered.encrypted_gratis_amount[10] ^= 1;
    assert!(decrypt_nod_for_owner(&view, &tampered).is_err());
}

fn binding() -> NetworkBindingV1 {
    NetworkBindingV1 {
        chain_id: U256::from(1u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
    }
}

fn resident_with(seed: u8, group: &[u8]) -> Arc<InProcessTeeContext> {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    context
        .activate_for_test(offer_material(group, B256::from(U256::from(1u64))))
        .unwrap();
    context
}

fn resident(seed: u8) -> Arc<InProcessTeeContext> {
    resident_with(seed, b"outbe-nod-s2w1-group-signature")
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

fn payload_parts(entry: &EnclaveWitnessEntryV1) -> (&Vec<u8>, B256, &Vec<u8>) {
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = entry
    else {
        panic!("recorded entry is not Payload");
    };
    (payload, *inputs_canonical_hash, attestation_tag)
}

fn flip_tag(entry: &mut EnclaveWitnessEntryV1) {
    let EnclaveWitnessEntryV1::Payload {
        attestation_tag, ..
    } = entry
    else {
        panic!("recorded entry is not Payload");
    };
    attestation_tag[0] ^= 1;
}

fn flip_hash(entry: &mut EnclaveWitnessEntryV1) {
    let EnclaveWitnessEntryV1::Payload {
        inputs_canonical_hash,
        ..
    } = entry
    else {
        panic!("recorded entry is not Payload");
    };
    let mut bytes = inputs_canonical_hash.0;
    bytes[0] ^= 1;
    *inputs_canonical_hash = B256::from(bytes);
}

fn replay_fault(
    leader: [u8; 32],
    tape: Vec<EnclaveWitnessEntryV1>,
    call: &impl Fn() -> Result<(), TransportError>,
    fault: BlockWitnessFault,
) {
    let _absent = set_in_process_tee_context_for_test(None);
    let (state, scope) = enter(WitnessExecutionMode::VerifierReplay, leader, tape);
    assert!(call().is_err());
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(witness.first_witness_fault, Some(fault));
    drop(witness);
    drop(scope);
}

fn prove_witness(
    tag: u8,
    honest: &impl Fn() -> Result<(), TransportError>,
    divergent: &impl Fn() -> Result<(), TransportError>,
    preimage: &[u8],
) {
    let leader_ctx = resident(4);
    let leader = leader_ctx.attestation_public();
    let _guard = install(&leader_ctx);
    let (state, scope) = enter(WitnessExecutionMode::ProposerRecord, leader, Vec::new());
    honest().unwrap();
    let tape = state.lock().unwrap().staged.clone();
    drop(scope);
    drop(_guard);
    let (payload, inputs, attestation_tag) = payload_parts(&tape[0]);
    assert_eq!(payload[0], tag);
    assert_ne!(inputs, B256::ZERO);
    assert_eq!(attestation_tag.len(), 64);

    let _absent = set_in_process_tee_context_for_test(None);
    let (state, scope) = enter(WitnessExecutionMode::VerifierReplay, leader, tape.clone());
    honest().unwrap();
    assert_eq!(state.lock().unwrap().prefix_cursor, 1);
    drop(scope);
    drop(_absent);

    let mut bad_tag = tape.clone();
    flip_tag(&mut bad_tag[0]);
    replay_fault(leader, bad_tag, honest, BlockWitnessFault::TapeInvalid);
    let mut bad_hash = tape.clone();
    flip_hash(&mut bad_hash[0]);
    replay_fault(leader, bad_hash, honest, BlockWitnessFault::TapeInvalid);

    let verifier = resident(9);
    assert_ne!(leader, verifier.attestation_public());
    let local_tag = sign_attestation_tag(&verifier, preimage);
    assert_ne!(local_tag.as_slice(), attestation_tag.as_slice());
    let _guard = install(&verifier);
    let (state, scope) = enter(
        WitnessExecutionMode::VerifierWithEnclave,
        leader,
        tape.clone(),
    );
    honest().unwrap();
    assert_eq!(state.lock().unwrap().prefix_cursor, 1);
    assert_eq!(state.lock().unwrap().first_witness_fault, None);
    drop(scope);
    drop(_guard);

    replay_fault(leader, tape, divergent, BlockWitnessFault::TapeInvalid);
}

fn resident_key() -> StateKeyHandle {
    derive_resident_state_key(&resident(4), &NOD_DOMAIN, 0).unwrap()
}

fn terms() -> NodTermsV1 {
    parse(TERMS)
}

fn load_request(amount: u64) -> NodGroupLoadOpRequestV1 {
    NodGroupLoadOpRequestV1 {
        chain_id: 1,
        scope: B256::repeat_byte(0x22),
        previous: None,
        operation: NodGroupLoadOperationV1::AdjustTransient {
            load_minor: U256::from(amount),
            add: true,
        },
    }
}

#[test]
fn witness_amount_group_load_and_quote_keep_leader_bytes() {
    let key = resident_key();
    let nod = encrypt_nod(&key, terms(), U256::from(17), U256::from(17)).unwrap();
    let other = encrypt_nod(&key, terms(), U256::from(18), U256::from(18)).unwrap();
    let inputs = nod_read_hash(&nod).unwrap();
    prove_witness(
        10,
        &|| read_nod_amount(&nod).map(|amount| assert_eq!(amount, U256::from(17))),
        &|| read_nod_amount(&other).map(|_| ()),
        &nod_read_preimage(inputs, U256::from(17)),
    );

    let request = load_request(17);
    let divergent_request = load_request(18);
    let record = apply_pure(&key, &request).unwrap();
    let inputs = nod_group_load_inputs_hash(&request).unwrap();
    let preimage = nod_group_load_attestation_preimage(inputs, &record).unwrap();
    prove_witness(
        19,
        &|| {
            apply_nod_group_load_operation(request.clone()).map(|applied| {
                assert_eq!(read_load_pure(&key, &applied).unwrap(), U256::from(17));
            })
        },
        &|| apply_nod_group_load_operation(divergent_request.clone()).map(|_| ()),
        &preimage,
    );

    let load = seal_group_load(
        &key,
        (1, B256::repeat_byte(0x22)),
        (0, B256::repeat_byte(0x44)),
        U256::from(17),
    )
    .unwrap();
    let other_load = seal_group_load(
        &key,
        (1, B256::repeat_byte(0x22)),
        (0, B256::repeat_byte(0x45)),
        U256::from(18),
    )
    .unwrap();
    let inputs = nod_group_load_read_hash(&load).unwrap();
    prove_witness(
        20,
        &|| read_nod_group_load(&load).map(|amount| assert_eq!(amount, U256::from(17))),
        &|| read_nod_group_load(&other_load).map(|_| ()),
        &nod_group_load_read_preimage(inputs, U256::from(17)),
    );

    let context: NodQuoteContextV1 = parse(QUOTE_CONTEXT);
    let payment = encrypt_nod_quote(&key, &nod, &context, U256::from(18)).unwrap();
    let inputs = nod_quote_inputs_hash(&nod, &context, U256::from(18)).unwrap();
    let preimage = attestation(b"outbe/nod/quote-encrypt-result/v1", inputs, &payment).unwrap();
    prove_witness(
        21,
        &|| {
            encrypt_quote_host(&nod, context.clone(), U256::from(18))
                .map(|bytes| assert_eq!(bytes, payment))
        },
        &|| encrypt_quote_host(&nod, context.clone(), U256::from(19)).map(|_| ()),
        &preimage,
    );
}

fn record(
    call: &impl Fn() -> Result<(), TransportError>,
) -> ([u8; 32], Vec<EnclaveWitnessEntryV1>) {
    let leader_ctx = resident(4);
    let leader = leader_ctx.attestation_public();
    let _guard = install(&leader_ctx);
    let (state, scope) = enter(WitnessExecutionMode::ProposerRecord, leader, Vec::new());
    call().unwrap();
    let tape = state.lock().unwrap().staged.clone();
    drop(scope);
    (leader, tape)
}

fn quote_payload(payment: Vec<u8>, inputs: B256) -> Vec<u8> {
    let mut payload = vec![21];
    payload.extend(
        postcard::to_allocvec(&NodQuoteEncryptedV1 {
            encrypted_payment: payment,
            inputs_canonical_hash: inputs,
            attestation_tag: Vec::new(),
        })
        .unwrap(),
    );
    payload
}

#[test]
fn replay_rejects_a_different_request_before_the_cursor_moves() {
    let key = resident_key();
    let nod = encrypt_nod(&key, terms(), U256::from(17), U256::from(17)).unwrap();
    let other = encrypt_nod(&key, terms(), U256::from(18), U256::from(18)).unwrap();
    let (leader, tape) = record(&|| read_nod_amount(&nod).map(|_| ()));
    replay_fault(
        leader,
        tape,
        &|| read_nod_amount(&other).map(|_| ()),
        BlockWitnessFault::TapeInvalid,
    );

    let request = load_request(17);
    let other_request = load_request(18);
    let (leader, tape) = record(&|| apply_nod_group_load_operation(request.clone()).map(|_| ()));
    replay_fault(
        leader,
        tape,
        &|| apply_nod_group_load_operation(other_request.clone()).map(|_| ()),
        BlockWitnessFault::TapeInvalid,
    );

    let load = seal_group_load(
        &key,
        (1, B256::repeat_byte(0x22)),
        (0, B256::repeat_byte(0x44)),
        U256::from(17),
    )
    .unwrap();
    let other_load = seal_group_load(
        &key,
        (1, B256::repeat_byte(0x22)),
        (0, B256::repeat_byte(0x45)),
        U256::from(18),
    )
    .unwrap();
    let (leader, tape) = record(&|| read_nod_group_load(&load).map(|_| ()));
    replay_fault(
        leader,
        tape,
        &|| read_nod_group_load(&other_load).map(|_| ()),
        BlockWitnessFault::TapeInvalid,
    );

    let context: NodQuoteContextV1 = parse(QUOTE_CONTEXT);
    let (leader, tape) =
        record(&|| encrypt_quote_host(&nod, context.clone(), U256::from(18)).map(|_| ()));
    replay_fault(
        leader,
        tape,
        &|| encrypt_quote_host(&nod, context.clone(), U256::from(19)).map(|_| ()),
        BlockWitnessFault::TapeInvalid,
    );
}

#[test]
fn foreign_state_key_rejects_the_same_request_before_the_cursor_moves() {
    let leader_key = resident_key();
    let foreign_ctx = resident_with(9, b"outbe-nod-s2w1-other-group-signature");
    let foreign_key = derive_resident_state_key(&foreign_ctx, &NOD_DOMAIN, 0).unwrap();
    let nod = encrypt_nod(&leader_key, terms(), U256::from(17), U256::from(17)).unwrap();
    let context: NodQuoteContextV1 = parse(QUOTE_CONTEXT);
    let request = load_request(17);
    let leader_record = apply_pure(&leader_key, &request).unwrap();
    let foreign_record = apply_pure(&foreign_key, &request).unwrap();
    assert_ne!(leader_record, foreign_record);
    assert!(decrypt_nod(&foreign_key, &nod).is_err());
    assert!(encrypt_nod_quote(&foreign_key, &nod, &context, U256::from(18)).is_err());

    let (leader, tape) = record(&|| read_nod_amount(&nod).map(|_| ()));
    replay_with_foreign(
        leader,
        tape,
        &|| read_nod_amount(&nod).map(|_| ()),
        BlockWitnessFault::LocalUnavailable,
    );

    let (leader, tape) = record(&|| apply_nod_group_load_operation(request.clone()).map(|_| ()));
    replay_with_foreign(
        leader,
        tape,
        &|| apply_nod_group_load_operation(request.clone()).map(|_| ()),
        BlockWitnessFault::SemanticMismatch,
    );

    let load = seal_group_load(
        &leader_key,
        (1, B256::repeat_byte(0x22)),
        (0, B256::repeat_byte(0x44)),
        U256::from(17),
    )
    .unwrap();
    let (leader, tape) = record(&|| read_nod_group_load(&load).map(|_| ()));
    replay_with_foreign(
        leader,
        tape,
        &|| read_nod_group_load(&load).map(|_| ()),
        BlockWitnessFault::LocalUnavailable,
    );

    let (leader, tape) =
        record(&|| encrypt_quote_host(&nod, context.clone(), U256::from(18)).map(|_| ()));
    replay_with_foreign(
        leader,
        tape,
        &|| encrypt_quote_host(&nod, context.clone(), U256::from(18)).map(|_| ()),
        BlockWitnessFault::LocalUnavailable,
    );
}

fn replay_with_foreign(
    leader: [u8; 32],
    tape: Vec<EnclaveWitnessEntryV1>,
    call: &impl Fn() -> Result<(), TransportError>,
    fault: BlockWitnessFault,
) {
    let foreign = resident_with(9, b"outbe-nod-s2w1-other-group-signature");
    let _guard = install(&foreign);
    let (state, scope) = enter(WitnessExecutionMode::VerifierWithEnclave, leader, tape);
    assert!(call().is_err());
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(witness.first_witness_fault, Some(fault));
    drop(witness);
    drop(scope);
}

#[test]
fn replay_rejects_a_signed_group_load_for_another_chain_scope_or_version() {
    let leader_ctx = resident(4);
    let leader = leader_ctx.attestation_public();
    let key = derive_resident_state_key(&leader_ctx, &NOD_DOMAIN, 0).unwrap();
    let request = load_request(17);
    let expected = nod_group_load_inputs_hash(&request).unwrap();
    let cases = [
        (2, request.scope, 0),
        (1, B256::repeat_byte(0x99), 0),
        (1, request.scope, 1),
    ];
    for (chain_id, scope, previous_version) in cases {
        let record = seal_group_load(
            &key,
            (chain_id, scope),
            (previous_version, expected),
            U256::from(17),
        )
        .unwrap();
        let preimage = nod_group_load_attestation_preimage(expected, &record).unwrap();
        let body = NodGroupLoadOpAppliedV1 {
            record,
            inputs_canonical_hash: expected,
            attestation_tag: Vec::new(),
        };
        let mut payload = vec![19];
        payload.extend(postcard::to_allocvec(&body).unwrap());
        replay_fault(
            leader,
            vec![EnclaveWitnessEntryV1::Payload {
                payload,
                inputs_canonical_hash: expected,
                attestation_tag: sign_attestation_tag(&leader_ctx, &preimage).to_vec(),
            }],
            &|| apply_nod_group_load_operation(request.clone()).map(|_| ()),
            BlockWitnessFault::TapeInvalid,
        );
    }
}

#[test]
fn encrypt_nod_quote_accepts_the_fixed_ed25519_vector() {
    let nod: EncryptedNodV1 = parse(NOD_FIXTURE);
    let context: NodQuoteContextV1 = parse(QUOTE_CONTEXT);
    let inputs = nod_quote_inputs_hash(&nod, &context, U256::from(18)).unwrap();
    assert_eq!(
        hex::encode(inputs),
        "b665b0b89642a4117ce7ae1b6565d2d33e1d3462046879c722c0b05188ca4263"
    );
    let leader: [u8; 32] =
        hex::decode("f80cccdce4ae1c07ae208a2adf99a310ae4207e0306fa0236110b06827bbb8d0")
            .unwrap()
            .try_into()
            .unwrap();
    let attestation_tag = hex::decode("b49c44dd138c16f09ad19fc25d34598ab2f9bc9ab638f8b714808db7403987b00221e7262d2410bef2d9aec6999bda74713610581e8aa0b722b3745320518c0b").unwrap();
    let _absent = set_in_process_tee_context_for_test(None);
    let (state, scope) = enter(
        WitnessExecutionMode::VerifierReplay,
        leader,
        vec![EnclaveWitnessEntryV1::Payload {
            payload: quote_payload(vec![65, 66], inputs),
            inputs_canonical_hash: inputs,
            attestation_tag,
        }],
    );
    assert_eq!(
        encrypt_quote_host(&nod, context, U256::from(18)).unwrap(),
        vec![65, 66]
    );
    assert_eq!(state.lock().unwrap().prefix_cursor, 1);
    assert_eq!(state.lock().unwrap().first_witness_fault, None);
    drop(scope);
}

fn flip_signed_tag(signed: &mut SignedNodHost, which: usize) {
    let tag = match which {
        0 => &mut signed.amount.attestation_tag,
        1 => &mut signed.applied.attestation_tag,
        2 => &mut signed.load.attestation_tag,
        _ => &mut signed.quote.attestation_tag,
    };
    assert_eq!(tag.len(), 64);
    tag[0] ^= 1;
}

#[test]
fn rpc_direct_authenticates_all_four_tags_without_staging() {
    let first = resident(4);
    let leader = first.attestation_public();
    let key = derive_resident_state_key(&first, &NOD_DOMAIN, 0).unwrap();
    let nod = encrypt_nod(&key, terms(), U256::from(17), U256::from(17)).unwrap();
    let request = load_request(17);
    let quote: NodQuoteContextV1 = parse(QUOTE_CONTEXT);
    let _guard = install(&first);
    let second = resident(9);
    assert_ne!(leader, second.attestation_public());
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::RpcDirect,
        Some(second.attestation_public()),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(Arc::clone(&state));
    assert!(current_witness_state().is_some());

    let amount = read_nod_amount(&nod).unwrap();
    let applied = apply_nod_group_load_operation(request.clone()).unwrap();
    let load_amount = read_nod_group_load(&applied).unwrap();
    let payment = encrypt_quote_host(&nod, quote.clone(), U256::from(18)).unwrap();
    assert_eq!(amount, U256::from(17));
    assert_eq!(load_amount, U256::from(17));
    assert!(!payment.is_empty());
    assert!(current_witness_state().is_some());

    let signed =
        sign_nod_host_for_test(&first, &nod, &request, &applied, (&quote, U256::from(18))).unwrap();
    assert_eq!(signed.amount.amount, amount);
    assert_eq!(signed.applied.record, applied);
    assert_eq!(signed.load.amount, load_amount);
    assert_eq!(signed.quote.encrypted_payment, payment);
    signed.authenticate(&leader).unwrap();

    replace_in_process_tee_context(&first, Arc::clone(&second), || {
        Ok::<(), InProcessDispatchError>(())
    })
    .unwrap();
    assert_eq!(
        in_process_tee_context().unwrap().attestation_public(),
        second.attestation_public()
    );
    assert_eq!(amount, U256::from(17));
    assert_eq!(load_amount, U256::from(17));
    assert_eq!(read_nod_amount(&nod).unwrap(), amount);
    assert_eq!(read_nod_group_load(&applied).unwrap(), load_amount);
    assert_eq!(
        encrypt_quote_host(&nod, quote.clone(), U256::from(18)).unwrap(),
        payment
    );
    assert_eq!(
        apply_nod_group_load_operation(request.clone()).unwrap(),
        applied
    );
    signed.authenticate(&leader).unwrap();
    let resigned =
        sign_nod_host_for_test(&second, &nod, &request, &applied, (&quote, U256::from(18)))
            .unwrap();
    assert_ne!(
        resigned.amount.attestation_tag,
        signed.amount.attestation_tag
    );
    assert_ne!(
        resigned.applied.attestation_tag,
        signed.applied.attestation_tag
    );
    assert_ne!(resigned.load.attestation_tag, signed.load.attestation_tag);
    assert_ne!(resigned.quote.attestation_tag, signed.quote.attestation_tag);
    assert!(signed.authenticate(&second.attestation_public()).is_err());
    assert!(current_witness_state().is_some());

    for which in 0..4 {
        let mut corrupt = signed.clone();
        flip_signed_tag(&mut corrupt, which);
        assert!(matches!(
            corrupt.authenticate(&leader),
            Err(TransportError::AppAttestation { .. })
        ));
    }
    let witness = state.lock().unwrap();
    assert!(witness.staged.is_empty());
    assert_eq!(witness.prefix_cursor, 0);
}

#[test]
fn read_nod_amount_rejects_a_mutated_owner() {
    let context = resident(4);
    let key = derive_resident_state_key(&context, &NOD_DOMAIN, 0).unwrap();
    let mut nod = encrypt_nod(&key, terms(), U256::from(17), U256::from(17)).unwrap();
    let _guard = install(&context);
    assert_eq!(read_nod_amount(&nod).unwrap(), U256::from(17));
    nod.terms.owner = Address::repeat_byte(9);
    assert!(read_nod_amount(&nod).is_err());
}

const NOD_CONTEXT_DIGEST: &str = "9f5ed554cb633715b10ef9300eee7572dd8d7bd9b7ee96f47b23f87152ae6720";
const NOD_AMOUNT_BLOB: &str = "000000000000000137f1780dda06389d4099f641acdc47b29f3834a3915aebccfda1eeb5cb1be0eb266f7e6a40d529099d3ea3df0ee44c17";
const NOD_WRONG_NONCE_DOMAIN_BLOB: &str = "0000000000000001b4cb27b25a97fb3f37dc0a8ee5525ce83bce9c4fdd1efb170fd412e8a76ea5bcd8feaf71b8df565d8b0d2b7d838b26cd";

fn fixed_nod() -> EncryptedNodV1 {
    let nod = EncryptedNodV1 {
        terms: NodTermsV1 {
            chain_id: 31_337,
            nod_id: U256::from_str_radix(
                "013527d8abababababababababababababababababababababababababababab",
                16,
            )
            .unwrap(),
            owner: Address::repeat_byte(0x42),
            emission_day: 20_260_824,
            entry_price_minor: U256::from(123_456u64),
            issuance_currency: 840,
            reference_currency: 978,
        },
        encryption_binding: B256::repeat_byte(0xa5),
        encrypted_nominal_amount: (hex::decode(NOD_AMOUNT_BLOB).unwrap()).clone(),
        encrypted_gratis_amount: hex::decode(NOD_AMOUNT_BLOB).unwrap(),
    };
    assert_eq!(hex::encode(nod.context_digest()), NOD_CONTEXT_DIGEST);
    nod
}

#[test]
fn fixed_nod_ciphertext_opens_the_exact_amount() {
    assert_eq!(
        decrypt_nod_for_owner(&[0x11; 32], &fixed_nod()).unwrap(),
        U256::from(987_654_321u64)
    );
}

#[test]
fn nod_decrypt_rejects_wrong_owner_binding_and_nonce_domain() {
    let view_key = [0x11; 32];
    let nod = fixed_nod();
    assert_eq!(
        decrypt_nod_for_owner(&[0x12; 32], &nod).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_context = nod.clone();
    wrong_context.terms.owner = Address::repeat_byte(0x43);
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &wrong_context).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_binding = nod.clone();
    wrong_binding.encryption_binding = B256::repeat_byte(0xa6);
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &wrong_binding).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_domain = nod;
    wrong_domain.encrypted_gratis_amount = hex::decode(NOD_WRONG_NONCE_DOMAIN_BLOB).unwrap();
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &wrong_domain).unwrap_err(),
        "NOD amount decryption failed"
    );
}

#[test]
fn nod_decrypt_rejects_bad_length_version_and_tag() {
    let view_key = [0x11; 32];
    let nod = fixed_nod();
    let mut short = nod.clone();
    short.encrypted_gratis_amount.pop();
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &short).unwrap_err(),
        "invalid encrypted NOD encoding"
    );
    let mut bad_version = nod.clone();
    bad_version.encrypted_gratis_amount[7] = 2;
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &bad_version).unwrap_err(),
        "invalid encrypted NOD encoding"
    );
    let mut bad_ciphertext = nod.clone();
    bad_ciphertext.encrypted_gratis_amount[8] ^= 1;
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &bad_ciphertext).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut bad_tag = nod;
    let tag_index = bad_tag.encrypted_gratis_amount.len() - 1;
    bad_tag.encrypted_gratis_amount[tag_index] ^= 1;
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &bad_tag).unwrap_err(),
        "NOD amount decryption failed"
    );
}
