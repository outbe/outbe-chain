//! Shared HKDF, ledger MAC, mutable-balance, and witness-envelope goldens.
//! HKDF answers were independently calculated with Python hashlib/hmac (RFC 5869).
use std::sync::{Arc, Mutex};

use alloy_primitives::{hex, Address, B256, U256};
use outbe_core_common::confidential_tee::{
    attestation, consensus_witness_call, decrypt_gratis_balance, decrypt_promis_balance,
    derive_account_keys_message, derive_resident_state_key, domain_for, eip191_hash,
    event_amount_input_hash, hkdf_sha256, offer_material, sign_attestation_tag,
    verify_response_attestation, BalanceTransition, DualWitnessAnswer, EnvelopeCipherSpec, Ledger,
    LocalCryptoError, ModifyAuthorization, ResponseAuthSpec, WitnessDomainWire, WitnessResponseTag,
    WitnessSchema, GRATIS_BALANCE_ENVELOPE, GRATIS_OP_ATTESTATION, PROMIS_BALANCE_ENVELOPE,
};
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_tee::resident::{AccountKeysHandle, StateKeyHandle};
use outbe_tee::witness::{
    BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode,
    WitnessExecutionScope, WitnessState,
};
use outbe_tee::{CallMeta, TransportError};

#[test]
fn owner_derive_account_keys_eip191_vectors() {
    for (ledger, preimage, digest) in [
        (Ledger::Gratis, "6f757462652f6772617469732f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "77543b6a6a9d609001a95b206671a7546f624787aa3ddad02db4ff553ae0a114"),
        (Ledger::Promis, "6f757462652f70726f6d69732f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "4358a5160a44f31ab9bd747e2829922960336f30d54af3d1e2b62c5b00708326"),
        (Ledger::Nod, "6f757462652f6e6f642f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "4bc1efbf18d8381d779979ea9dfdb8d87b1cdb2acc6cef8b42ee5ef60652af8b"),
    ] {
        let message = derive_account_keys_message(
            ledger,
            Address::repeat_byte(0x22),
            B256::repeat_byte(0x33),
        );
        assert_eq!(hex::encode(&message), preimage);
        assert_eq!(hex::encode(eip191_hash(&message)), digest);
    }
}

#[test]
fn confidential_ledger_domain_vectors() {
    let request = ModifyAuthorization {
        account: Address::repeat_byte(0x22),
        op_tag: 3,
        amount: U256::from(17),
        op_nonce: 5,
        chain_id: B256::repeat_byte(0x11),
    };
    for (ledger, state, view, modify, mac) in [
        (
            Ledger::Gratis,
            "e013387bceef27a343ed0a944ad6da239cbd3417b3bf6d5013e22b67eca56aed",
            "34e065f26cd8de1bef9cdbee177dd98fecc503548706012a462014e77ba4ce9a",
            "743337cd82800c7d896ce426a094fb0f1a2280a5d30d8af35fb45860ea2cca57",
            "9b134699089490db390711a5c5e4c8d648ed168babad9c4105c17a36655c882b",
        ),
        (
            Ledger::Promis,
            "d50d5b049862d893c48d0991e5fcff5b8d45b573b323905c0cdae3778a895f36",
            "c3bd3c43c45fcdad0093cb3f6758b3f69c4a16c27e0c5e50c68d53079c1dabfb",
            "0320763f3ed5479059f29142165691624c3849c9b869b403ae354ab31d58b3eb",
            "d8704b22c0854b641b9cae741c4c7f995c3e5ee7b305a3bb78ba42d95d869afc",
        ),
        (
            Ledger::Nod,
            "f068ed0d51419d40863b308b94592ae3dee981b77ad94c8eb7b97f819c061410",
            "2754605a328a631e5caa9f0da9bc07366b624b761dcbd7c92ac19f14805e97e1",
            "2c9e9247c5b980297f47a7154fee2372e015efc36ca21960705d49c79425ccc6",
            "7d3d88e09726be021d3b4ae05fd8cf83ae3e65dcc8a87e5c56d50a089491ea4c",
        ),
    ] {
        let domain = domain_for(ledger);
        let state_key = domain
            .derive_state_key(
                &offer_material(&[0x44; 32], B256::repeat_byte(0x11)),
                B256::repeat_byte(0x11),
                7,
            )
            .unwrap();
        assert_key_vector(&state_key, state);
        let view_key = domain
            .account_keys
            .derive_view_key(&state_key, request.account)
            .unwrap();
        let modify_key = domain
            .account_keys
            .derive_account_keys(&state_key, request.account)
            .unwrap();
        assert_key_vector(&view_key, view);
        let modify_bytes: [u8; 32] = hex::decode(modify).unwrap().try_into().unwrap();
        assert_eq!(
            domain.authorization.modify_mac(&modify_key, &request),
            domain.authorization.modify_mac(
                &AccountKeysHandle::for_test(request.account, [0; 32], modify_bytes),
                &request
            ),
        );
        assert_eq!(
            hex::encode(domain.authorization.modify_mac(&modify_key, &request)),
            mac
        );
    }
}

#[test]
fn tee_identity_kdf_reference_vectors() {
    for (info, output) in [
        (
            b"outbe/tee/noise-static/v1".as_slice(),
            "6b9b55813d4152187af7c8c4c9e26b367ff6f2a1c8915cdc9c23bc9b0ea2b945",
        ),
        (
            b"outbe/tee/recipient-x25519/v1".as_slice(),
            "d1aaf721ba2468cbf4da3120453d68a4eb095028dea9ddbf17922d152e44a69e",
        ),
        (
            b"outbe/tee/attestation-ed25519/v1".as_slice(),
            "3107bac161498f5fc7604daba904fb44f7c445cb43eff88241390e37edc9f585",
        ),
        (
            b"outbe/tee/dkg-enc/v1".as_slice(),
            "8690d36dae706b74d992dc4d7b4c6a761b5c4945a5185dcad3b6b910a8941fce",
        ),
        (
            b"outbe/tee/dkg-bls-seed/v1".as_slice(),
            "9b0b2b00e3dd2a234db5bcc2e573f28ea74e5c56edc80a37cab8c793ffccba15",
        ),
    ] {
        let seed: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap());
        assert_eq!(hex::encode(hkdf_sha256(&seed, b"", info).unwrap()), output);
    }
}

#[test]
fn production_attestation_identity_public_key_vector() {
    let identity_seed: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap());
    let key = outbe_tee::resident::ResidentTeeKeys::from_seed(identity_seed).unwrap();
    assert_eq!(
        hex::encode(key.attestation_pub()),
        "4eca380880bfe19d2e38151e9c97aade3a8b9f7c169c1b212af5e57a78dd9ace"
    );
}

#[test]
fn mutable_balance_cipher_vectors() {
    let transition = || BalanceTransition {
        account: Address::repeat_byte(0x22),
        field: 0,
        previous_version: 4,
        amount: U256::from(17),
        input_hash: B256::repeat_byte(0x33),
    };
    assert_balance_vector(
        &GRATIS_BALANCE_ENVELOPE,
        decrypt_gratis_balance,
        "000000000000000547524132487af11f7a8410e9cc3c0a1e9ed6a40e2e322e9c6f68aff3a10870102fb4973f3ea68cb7a480699247f9259e9915748b1df8c1aeff474f9bf56851dc7534672f365421ed5f4e4de356aa5f9ed900186a",
        &transition(),
    );
    assert_balance_vector(
        &PROMIS_BALANCE_ENVELOPE,
        decrypt_promis_balance,
        "000000000000000550524f32a0da28be8b5eae47acd5ff7d780f9af69f684ae69bf79607a10669cae33289fe0f9aa44218f93e4fff57a2be4de213b4f586d48ef0fbb007619920a7c46774c710a9cfe3f7d31af4452aefcac0d4904c",
        &transition(),
    );
}

type OwnerBalanceRead = fn(&[u8; 32], Address, &[u8]) -> Result<U256, LocalCryptoError>;

fn assert_balance_vector(
    spec: &EnvelopeCipherSpec,
    read: OwnerBalanceRead,
    expected: &str,
    transition: &BalanceTransition,
) {
    let actual = spec
        .write_amount(
            &StateKeyHandle::for_test([0x11; 32]),
            BalanceTransition {
                account: transition.account,
                field: transition.field,
                previous_version: transition.previous_version,
                amount: transition.amount,
                input_hash: transition.input_hash,
            },
        )
        .unwrap();
    assert_eq!(hex::encode(actual), expected);
    assert_eq!(
        read(
            &[0x11; 32],
            Address::repeat_byte(0x22),
            &hex::decode(expected).unwrap()
        )
        .unwrap(),
        U256::from(17)
    );
}

#[test]
fn resident_state_key_matches_domain_derivation() {
    let binding = binding();
    let chain = B256::from(binding.chain_id);
    let context = outbe_tee::test_utils::resident_context(4, binding);
    context
        .activate_for_test(offer_material(&[0x44; 32], chain))
        .unwrap();
    let via_context = derive_resident_state_key(&context, domain_for(Ledger::Gratis), 7).unwrap();
    let via_domain = domain_for(Ledger::Gratis)
        .derive_state_key(&offer_material(&[0x44; 32], chain), chain, 7)
        .unwrap();
    assert_eq!(
        via_context.compute_test_mac(b"golden", b"key parity"),
        via_domain.compute_test_mac(b"golden", b"key parity")
    );
}

#[test]
fn resident_attestation_tag_round_trips() {
    let context = outbe_tee::test_utils::resident_context(4, binding());
    let inputs = B256::repeat_byte(0x61);
    let preimage = attestation(b"outbe/gratis/op", inputs, "ok").unwrap();
    let tag = sign_attestation_tag(&context, &preimage);
    let public = context.attestation_public();
    verify_response_attestation(
        &ResponseAuthSpec {
            attestation_pub: &public,
            descriptor: GRATIS_OP_ATTESTATION,
            expected_inputs_hash: inputs,
            actual_inputs_hash: inputs,
            tag: &tag,
        },
        &preimage,
    )
    .unwrap();
}

/// Independent `(variant, discriminant)` table copied from the frozen V1
/// response golden. A swapped enum discriminant fails `literal`, not `value()`.
const RECORDABLE_V1_TAG_LITERALS: [(WitnessResponseTag, u8); 9] = [
    (WitnessResponseTag::GratisOpApplied, 1),
    (WitnessResponseTag::PromisOpApplied, 2),
    (WitnessResponseTag::EncryptedNodMintRejectedV1, 8),
    (WitnessResponseTag::EncryptedNodMinedV1, 9),
    (WitnessResponseTag::NodAmountReadV1, 10),
    (WitnessResponseTag::NodGroupLoadOpAppliedV1, 19),
    (WitnessResponseTag::NodGroupLoadReadV1, 20),
    (WitnessResponseTag::NodQuoteEncryptedV1, 21),
    (WitnessResponseTag::NodIssuedAtOfferV1, 22),
];

#[test]
fn recordable_v1_tags_match_independent_literals() {
    assert_eq!(
        RECORDABLE_V1_TAG_LITERALS.len(),
        WitnessResponseTag::ALL.len()
    );
    for (index, (variant, literal)) in RECORDABLE_V1_TAG_LITERALS.iter().copied().enumerate() {
        assert_eq!(WitnessResponseTag::ALL[index], variant);
        assert_eq!(variant.value(), literal);
        assert_eq!(WitnessResponseTag::try_from(literal).unwrap(), variant);
    }
}

#[test]
fn event_amount_input_hash_vector() {
    assert_eq!(
        hex::encode(event_amount_input_hash(B256::repeat_byte(0x33))),
        "5dea5dcd766111745d06eeca38638fc31594f1e02d93f02f65a13bd9beb4c541"
    );
}

const GRATIS_OP_APPLIED_SIGNED: &str = "010001210122200000000000000000000000000000000000000000000000000000000000000011052011111111111111111111111111111111111111111111111111111111111111110251520123";
const NOD_AMOUNT_READ_SIGNED: &str = "0a200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152";
const NOD_MINED_SIGNED: &str =
    "090121052011111111111111111111111111111111111111111111111111111111111111110251520123";
const NOD_MINT_REJECTED_SIGNED: &str =
    "080872656a6563746564201111111111111111111111111111111111111111111111111111111111111111025152";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum GratisStatusWire {
    Applied,
    Rejected { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct GratisOpAppliedWire {
    status: GratisStatusWire,
    new_balance: Vec<u8>,
    new_pledged: Vec<u8>,
    event_amount: U256,
    next_op_nonce: u64,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
    encrypted_event_amount: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct NodAmountReadWire {
    amount: U256,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct NodMinedWire {
    new_balance: Vec<u8>,
    next_op_nonce: u64,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
    encrypted_event_amount: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct NodMintRejectedWire {
    reason: String,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SignedAmount {
    amount: u64,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

macro_rules! schema_wire {
    ($ty:ty, $tag:expr) => {
        impl WitnessSchema for $ty {
            const TAG: WitnessResponseTag = $tag;
            fn inputs_canonical_hash(&self) -> B256 {
                self.inputs_canonical_hash
            }
            fn attestation_tag(&self) -> &[u8] {
                &self.attestation_tag
            }
            fn set_attestation_tag(&mut self, tag: Vec<u8>) {
                self.attestation_tag = tag;
            }
        }
    };
}

schema_wire!(GratisOpAppliedWire, WitnessResponseTag::GratisOpApplied);
schema_wire!(NodAmountReadWire, WitnessResponseTag::NodAmountReadV1);
schema_wire!(NodMinedWire, WitnessResponseTag::EncryptedNodMinedV1);
schema_wire!(
    NodMintRejectedWire,
    WitnessResponseTag::EncryptedNodMintRejectedV1
);
schema_wire!(SignedAmount, WitnessResponseTag::GratisOpApplied);

fn frozen_tag() -> Vec<u8> {
    vec![81, 82]
}

fn inputs_hash() -> B256 {
    B256::repeat_byte(0x11)
}

fn tag_free_golden(signed_hex: &str) -> Vec<u8> {
    let bytes = hex::decode(signed_hex).unwrap();
    let at = bytes
        .windows(3)
        .position(|window| window == [2, 0x51, 0x52])
        .expect("one attestation vec");
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

fn accept_frozen_tag<T: WitnessDomainWire>(
    _key: &[u8; 32],
    value: &T,
) -> Result<(), TransportError> {
    if value.attestation_tag() == [81, 82] {
        return Ok(());
    }
    Err(TransportError::UnexpectedResponse)
}

fn record_layout<T>(value: T, signed_hex: &str)
where
    T: WitnessDomainWire + PartialEq + std::fmt::Debug,
{
    let expected = tag_free_golden(signed_hex);
    let (_guard, leader) = install_context();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    assert_eq!(
        consensus_witness_call(
            CallMeta::recorded("v1_layout"),
            {
                let value = value.clone();
                move |_| Ok(value)
            },
            accept_frozen_tag,
        )
        .unwrap(),
        value
    );
    let staged = state.lock().unwrap().staged.clone();
    drop(_scope);
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &staged[0]
    else {
        panic!("recorded entry is not Payload");
    };
    assert_eq!(payload, &expected);
    assert_eq!(*inputs_canonical_hash, inputs_hash());
    assert_eq!(attestation_tag.as_slice(), [81, 82]);

    let replay = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: staged,
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(replay.clone());
    let replayed = consensus_witness_call(
        CallMeta::recorded("v1_layout"),
        |_| -> Result<T, TransportError> { panic!("replay executed") },
        accept_frozen_tag,
    )
    .unwrap();
    assert_eq!(replayed, value);
    assert_eq!(replayed.attestation_tag(), &[81, 82]);
    assert_eq!(replay.lock().unwrap().prefix_cursor, 1);
}

#[test]
fn v1_payloads_match_tag_free_response_vectors() {
    let hash = inputs_hash();
    let tag = frozen_tag();
    record_layout(
        GratisOpAppliedWire {
            status: GratisStatusWire::Applied,
            new_balance: vec![33],
            new_pledged: vec![34],
            event_amount: U256::from(0x11u64),
            next_op_nonce: 5,
            inputs_canonical_hash: hash,
            attestation_tag: tag.clone(),
            encrypted_event_amount: vec![35],
        },
        GRATIS_OP_APPLIED_SIGNED,
    );
    record_layout(
        NodAmountReadWire {
            amount: U256::from(0x11u64),
            inputs_canonical_hash: hash,
            attestation_tag: tag.clone(),
        },
        NOD_AMOUNT_READ_SIGNED,
    );
    record_pair(
        NodMinedWire {
            new_balance: vec![33],
            next_op_nonce: 5,
            inputs_canonical_hash: hash,
            attestation_tag: tag.clone(),
            encrypted_event_amount: vec![35],
        },
        NodMintRejectedWire {
            reason: "rejected".to_string(),
            inputs_canonical_hash: hash,
            attestation_tag: tag,
        },
        NOD_MINED_SIGNED,
        NOD_MINT_REJECTED_SIGNED,
    );
}

fn record_pair<A, R>(applied: A, rejected: R, applied_hex: &str, rejected_hex: &str)
where
    A: WitnessSchema + PartialEq + std::fmt::Debug,
    R: WitnessSchema + PartialEq + std::fmt::Debug,
{
    record_layout(DualWitnessAnswer::<A, R>::Applied(applied), applied_hex);
    record_layout(DualWitnessAnswer::<A, R>::Rejected(rejected), rejected_hex);
}

fn binding() -> NetworkBindingV1 {
    NetworkBindingV1 {
        chain_id: U256::from(676).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
    }
}

fn install_context() -> (outbe_tee::in_process_dispatch::InProcessTestGuard, [u8; 32]) {
    let context = Arc::new(outbe_tee::test_utils::resident_context(4, binding()));
    let leader = context.attestation_public();
    let guard = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(context));
    (guard, leader)
}

const WITNESS_DOMAIN: &[u8] = b"outbe/witness/golden/v1";

fn signed_amount(amount: u64, inputs: B256) -> SignedAmount {
    SignedAmount {
        amount,
        inputs_canonical_hash: inputs,
        attestation_tag: Vec::new(),
    }
}

fn sign_amount(
    context: &outbe_tee::in_process::InProcessTeeContext,
    mut body: SignedAmount,
) -> Result<SignedAmount, TransportError> {
    body.attestation_tag.clear();
    let preimage = attestation(WITNESS_DOMAIN, body.inputs_canonical_hash, &body)
        .map_err(TransportError::codec)?;
    body.attestation_tag = sign_attestation_tag(context, &preimage).to_vec();
    Ok(body)
}

fn authenticate_amount(key: &[u8; 32], value: &SignedAmount) -> Result<(), TransportError> {
    let mut unsigned = value.clone();
    unsigned.attestation_tag.clear();
    let preimage = attestation(WITNESS_DOMAIN, value.inputs_canonical_hash, &unsigned)
        .map_err(TransportError::codec)?;
    verify_response_attestation(
        &ResponseAuthSpec {
            attestation_pub: key,
            descriptor: GRATIS_OP_ATTESTATION,
            expected_inputs_hash: value.inputs_canonical_hash,
            actual_inputs_hash: value.inputs_canonical_hash,
            tag: &value.attestation_tag,
        },
        &preimage,
    )
}

fn record_signed(seed: u8, value: SignedAmount) -> (Vec<EnclaveWitnessEntryV1>, [u8; 32]) {
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
    let key = context.attestation_public();
    let _guard = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(context));
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(key),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let recorded = consensus_witness_call(
        CallMeta::recorded("golden_signed"),
        {
            let value = value.clone();
            move |context| sign_amount(context, value)
        },
        authenticate_amount,
    )
    .unwrap();
    assert_eq!(recorded.amount, value.amount);
    assert_eq!(recorded.inputs_canonical_hash, value.inputs_canonical_hash);
    let staged = state.lock().unwrap().staged.clone();
    (staged, key)
}

#[test]
fn signed_witness_layout_replays_and_rejects_one_bit() {
    let inputs = B256::repeat_byte(0x5a);
    let body = signed_amount(11, inputs);
    let (staged, leader) = record_signed(4, body.clone());
    let leader_ctx = outbe_tee::test_utils::resident_context(4, binding());
    let preimage = attestation(WITNESS_DOMAIN, inputs, &body).unwrap();
    let leader_sig = sign_attestation_tag(&leader_ctx, &preimage);
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &staged[0]
    else {
        panic!("recorded entry is not Payload");
    };
    let mut expected = vec![WitnessResponseTag::GratisOpApplied.value()];
    expected.extend(postcard::to_allocvec(&body).unwrap());
    assert_eq!(payload, &expected);
    assert_eq!(*inputs_canonical_hash, inputs);
    assert_eq!(attestation_tag.as_slice(), leader_sig.as_slice());

    let replay = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: staged.clone(),
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(replay.clone());
    let value = consensus_witness_call(
        CallMeta::recorded("golden_signed"),
        |_| -> Result<SignedAmount, TransportError> { panic!("replay executed") },
        authenticate_amount,
    )
    .unwrap();
    assert_eq!(value.amount, body.amount);
    assert_eq!(value.attestation_tag.as_slice(), leader_sig.as_slice());
    assert_eq!(replay.lock().unwrap().prefix_cursor, 1);
    drop(_scope);

    for corrupt_tag in [true, false] {
        let mut bad = staged.clone();
        let EnclaveWitnessEntryV1::Payload {
            attestation_tag,
            inputs_canonical_hash,
            ..
        } = &mut bad[0]
        else {
            panic!("recorded entry is not Payload");
        };
        if corrupt_tag {
            attestation_tag[0] ^= 0x01;
        } else {
            let mut bytes = inputs_canonical_hash.0;
            bytes[0] ^= 0x01;
            *inputs_canonical_hash = B256::from(bytes);
        }
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierReplay,
            Some(leader),
            EnclaveWitnessTapeV1 {
                prefix: bad,
                txs: Vec::new(),
            },
        )));
        let _scope = WitnessExecutionScope::enter(state.clone());
        assert!(consensus_witness_call(
            CallMeta::recorded("golden_signed"),
            |_| -> Result<SignedAmount, TransportError> { panic!("corrupt replay executed") },
            authenticate_amount,
        )
        .is_err());
        let witness = state.lock().unwrap();
        assert_eq!(witness.prefix_cursor, 0);
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::TapeInvalid)
        );
    }
}

#[test]
fn distinct_validator_keys_ignore_attestation_tag_and_reject_divergence() {
    let inputs = B256::repeat_byte(0x5a);
    let body = signed_amount(11, inputs);
    let (staged, leader) = record_signed(4, body.clone());
    let verifier = Arc::new(outbe_tee::test_utils::resident_context(9, binding()));
    assert_ne!(leader, verifier.attestation_public());
    let preimage = attestation(WITNESS_DOMAIN, inputs, &body).unwrap();
    let leader_sig = sign_attestation_tag(
        &outbe_tee::test_utils::resident_context(4, binding()),
        &preimage,
    );
    assert_ne!(
        leader_sig.as_slice(),
        sign_attestation_tag(verifier.as_ref(), &preimage).as_slice()
    );
    let _guard =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(verifier));
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: staged.clone(),
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let value = consensus_witness_call(
        CallMeta::recorded("golden_distinct"),
        {
            let body = body.clone();
            move |context| sign_amount(context, body)
        },
        authenticate_amount,
    )
    .unwrap();
    assert_eq!(value.amount, body.amount);
    assert_eq!(value.attestation_tag.as_slice(), leader_sig.as_slice());
    assert_eq!(state.lock().unwrap().prefix_cursor, 1);
    drop(_scope);
    for live in [
        signed_amount(12, inputs),
        signed_amount(11, B256::repeat_byte(0x5b)),
    ] {
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierWithEnclave,
            Some(leader),
            EnclaveWitnessTapeV1 {
                prefix: staged.clone(),
                txs: Vec::new(),
            },
        )));
        let _scope = WitnessExecutionScope::enter(state.clone());
        assert!(consensus_witness_call(
            CallMeta::recorded("golden_diverge"),
            {
                let live = live.clone();
                move |context| sign_amount(context, live)
            },
            authenticate_amount,
        )
        .is_err());
        let witness = state.lock().unwrap();
        assert_eq!(witness.prefix_cursor, 0);
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::SemanticMismatch)
        );
    }
}

fn assert_key_vector(handle: &StateKeyHandle, expected: &str) {
    let expected = StateKeyHandle::for_test(hex::decode(expected).unwrap().try_into().unwrap());
    assert_eq!(
        handle.compute_test_mac(b"golden", b"key parity"),
        expected.compute_test_mac(b"golden", b"key parity")
    );
}
