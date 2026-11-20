//! Known-answer, rejection, and conservation checks for the Promis engine.

use alloy_primitives::{address, b256, Address, B256, U256};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use outbe_core_common::confidential_tee::{ModifyAuthorization, GRATIS, PROMIS};
use outbe_tee::resident::{AccountKeysHandle, ResidentOfferMaterial, StateKeyHandle};
use outbe_tee::TeeAttestationError;

use crate::engine::{
    apply_op, decrypt_balance, derive_account_keys, derive_promis_state_key, derive_view_key,
    modify_mac, ModifyOperation,
};
use crate::protocol::{
    promis_op_attestation_preimage, ModifyAuth, PromisOp, PromisOpRequest, PromisOpStatus,
};
use crate::{verify_promis_op_attestation, PROMIS_OP_ATTESTATION};

const CHAIN: B256 = B256::repeat_byte(0xC2);
const GROUP: &[u8] = b"a-group-threshold-signature-~48-bytes-long!!";

fn state_key() -> StateKeyHandle {
    let material = ResidentOfferMaterial::for_test(GROUP.to_vec(), CHAIN, 0, 0).unwrap();
    derive_promis_state_key(&material, CHAIN, 0).unwrap()
}

fn alice() -> Address {
    Address::repeat_byte(0x11)
}

fn auth(
    sk: &StateKeyHandle,
    account: Address,
    op: PromisOp,
    amount: U256,
    nonce: u64,
) -> ModifyAuth {
    let keys = derive_account_keys(sk, account).unwrap();
    ModifyAuth {
        mac: modify_mac(
            &keys,
            &ModifyOperation {
                account,
                op,
                amount,
                op_nonce: nonce,
                chain_id: CHAIN,
            },
        ),
        op_nonce: nonce,
    }
}

fn request(op: PromisOp, account: Address, amount: U256, nonce: u64) -> PromisOpRequest {
    PromisOpRequest {
        op,
        chain_id: CHAIN,
        account,
        amount,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth {
            mac: [0u8; 32],
            op_nonce: nonce,
        },
    }
}

fn reason(status: &PromisOpStatus) -> &str {
    match status {
        PromisOpStatus::Rejected { reason } => reason,
        PromisOpStatus::Applied => panic!("expected a rejection"),
    }
}

#[test]
fn mint_is_deterministic_across_calls() {
    let sk = state_key();
    let mut req = request(PromisOp::Mint, alice(), U256::from(1000u64), 0);
    req.modify_auth = auth(&sk, alice(), PromisOp::Mint, req.amount, 0);
    let first = apply_op(&sk, &req);
    let second = apply_op(&sk, &req);
    assert_eq!(first, second);
    assert_eq!(first.status, PromisOpStatus::Applied);
}

#[test]
fn view_key_decrypts_minted_balance() {
    let sk = state_key();
    let mut req = request(PromisOp::Mint, alice(), U256::from(4242u64), 0);
    req.modify_auth = auth(&sk, alice(), PromisOp::Mint, req.amount, 0);
    let result = apply_op(&sk, &req);
    let view = derive_view_key(&sk, alice()).unwrap();
    assert_eq!(
        decrypt_balance(&view, alice(), &result.new_balance).unwrap(),
        U256::from(4242u64)
    );
}

#[test]
fn modify_nonce_exhaustion_rejects() {
    let sk = state_key();
    let mut req = request(PromisOp::Mint, alice(), U256::from(1u64), u64::MAX);
    req.modify_auth = auth(&sk, alice(), PromisOp::Mint, req.amount, u64::MAX);
    assert_eq!(
        reason(&apply_op(&sk, &req).status),
        "modify nonce exhausted"
    );
}

#[test]
fn mint_rejects_forged_modify_auth() {
    let sk = state_key();
    let mut req = request(PromisOp::Mint, alice(), U256::from(1u64), 0);
    req.modify_auth = ModifyAuth {
        mac: [7u8; 32],
        op_nonce: 0,
    };
    assert_eq!(
        reason(&apply_op(&sk, &req).status),
        "invalid modify authorization"
    );
}

#[test]
fn zero_amount_and_zero_account_reject_before_the_balance_moves() {
    let sk = state_key();
    let zero_amount = apply_op(&sk, &request(PromisOp::Mint, alice(), U256::ZERO, 0));
    assert_eq!(reason(&zero_amount.status), "amount must be positive");
    assert!(zero_amount.new_balance.is_empty());
    assert_eq!(zero_amount.next_op_nonce, 0);
    let mut zero_account = request(PromisOp::Mint, Address::ZERO, U256::from(1u64), 0);
    zero_account.modify_auth = auth(&sk, Address::ZERO, PromisOp::Mint, zero_account.amount, 0);
    assert_eq!(
        reason(&apply_op(&sk, &zero_account).status),
        "invalid address"
    );
}

#[test]
fn burn_requires_sufficient_balance_and_a_valid_burn_reduces_it() {
    let sk = state_key();
    let mut mint = request(PromisOp::Mint, alice(), U256::from(1000u64), 0);
    mint.modify_auth = auth(&sk, alice(), PromisOp::Mint, mint.amount, 0);
    let minted = apply_op(&sk, &mint);
    let mut too_big = request(PromisOp::Burn, alice(), U256::from(200u64), 1);
    too_big.current_balance = minted.new_balance.clone();
    too_big.modify_auth = auth(&sk, alice(), PromisOp::Burn, too_big.amount, 1);
    too_big.amount = U256::from(2000u64);
    too_big.modify_auth = auth(&sk, alice(), PromisOp::Burn, too_big.amount, 1);
    let rejected = apply_op(&sk, &too_big);
    assert_eq!(reason(&rejected.status), "insufficient balance");
    assert!(rejected.new_balance.is_empty());
    assert_eq!(rejected.next_op_nonce, 0);

    let mut burn = request(PromisOp::Burn, alice(), U256::from(300u64), 1);
    burn.current_balance = minted.new_balance;
    burn.modify_auth = auth(&sk, alice(), PromisOp::Burn, burn.amount, 1);
    let burned = apply_op(&sk, &burn);
    assert_eq!(burned.status, PromisOpStatus::Applied);
    let view = derive_view_key(&sk, alice()).unwrap();
    assert_eq!(
        decrypt_balance(&view, alice(), &burned.new_balance).unwrap(),
        U256::from(700u64)
    );
}

#[test]
fn foreign_chain_is_a_signed_mac_rejection() {
    let sk = state_key();
    let mut req = request(PromisOp::Mint, alice(), U256::from(10u64), 0);
    req.modify_auth = auth(&sk, alice(), PromisOp::Mint, req.amount, 0);
    req.chain_id = B256::repeat_byte(0x5a);
    let rejected = apply_op(&sk, &req);
    assert_eq!(reason(&rejected.status), "invalid modify authorization");
    assert_ne!(rejected.inputs_canonical_hash, B256::ZERO);
    assert!(rejected.new_balance.is_empty());
}

#[test]
fn modify_mac_binds_every_input_and_is_separated_from_the_gratis_domain() {
    let sk = state_key();
    let keys = derive_account_keys(&sk, alice()).unwrap();
    let base = authorization(alice(), PromisOp::Mint as u8, 10, 3, CHAIN);
    let mac = PROMIS.authorization.modify_mac(&keys, &base);
    let variants = [
        authorization(
            Address::repeat_byte(0x22),
            PromisOp::Mint as u8,
            10,
            3,
            CHAIN,
        ),
        authorization(alice(), PromisOp::Burn as u8, 10, 3, CHAIN),
        authorization(alice(), PromisOp::Mint as u8, 11, 3, CHAIN),
        authorization(alice(), PromisOp::Mint as u8, 10, 4, CHAIN),
        authorization(
            alice(),
            PromisOp::Mint as u8,
            10,
            3,
            B256::repeat_byte(0xC3),
        ),
        authorization(alice(), PromisOp::Mint as u8, 10, 3, CHAIN),
    ];
    for (index, other) in variants.iter().enumerate().take(5) {
        let other_mac = PROMIS.authorization.modify_mac(&keys, other);
        assert_ne!(mac, other_mac, "variant {index}");
        assert!(!PROMIS
            .authorization
            .verify_modify_auth(&keys, &base, &other_mac));
    }
    let gratis_mac = GRATIS.authorization.modify_mac(&keys, &variants[5]);
    assert_ne!(mac, gratis_mac);
    assert!(!PROMIS
        .authorization
        .verify_modify_auth(&keys, &base, &gratis_mac));
    assert!(PROMIS.authorization.verify_modify_auth(&keys, &base, &mac));
}

fn authorization(
    account: Address,
    op_tag: u8,
    amount: u64,
    op_nonce: u64,
    chain_id: B256,
) -> ModifyAuthorization {
    ModifyAuthorization {
        account,
        op_tag,
        amount: U256::from(amount),
        op_nonce,
        chain_id,
    }
}

#[test]
fn modify_mac_matches_the_known_answer_vectors() {
    let chain_one = B256::from(U256::from(1u64));
    let rows = [
        (
            &GRATIS,
            alice(),
            0u8,
            U256::from(1_000u64),
            0u64,
            chain_one,
            b256!("0xec11651be45952198f63a0c982bbe045c3f3a31131c8afe655f76b243dc50aa1"),
        ),
        (
            &GRATIS,
            alice(),
            2,
            U256::from(10_000_000u64),
            3,
            chain_one,
            b256!("0xf7d19e989e00dc99df7168cbca111ac11c7328fa0e12c4b0e7873b3ce4e84046"),
        ),
        (
            &PROMIS,
            alice(),
            PromisOp::Mint as u8,
            U256::from(1_000u64),
            0,
            chain_one,
            b256!("0x3d33ac4e239f595c5d87e9db9bd4e180ff4f080811f22150cfc0b7b7eba2467b"),
        ),
        (
            &PROMIS,
            address!("0xabcdef0123456789abcdef0123456789abcdef01"),
            PromisOp::Burn as u8,
            U256::from(123_456_789_012_345_678_901_234_567_890u128),
            7,
            CHAIN,
            b256!("0x94bbfa7708394cf45466594357d2dc942f65e92ef0aa2509e03dfec5a5861f84"),
        ),
        (
            &PROMIS,
            address!("0x63467B02a7382408A845a5EB85b5238b8a4dD0eD"),
            PromisOp::Mint as u8,
            U256::from(7_000_000u64),
            9,
            b256!("0x00000000000000000000000000000000000000000000000000000000033ce4a9"),
            b256!("0x7b3938033a1bb7b1ef93540f3fb86d01122bc8772b2ef4a8d1f6d3377ee1e02d"),
        ),
    ];
    for (ledger, account, op_tag, amount, op_nonce, chain_id, expected) in rows {
        let modify = if expected
            == b256!("0x7b3938033a1bb7b1ef93540f3fb86d01122bc8772b2ef4a8d1f6d3377ee1e02d")
        {
            [0x6b; 32]
        } else {
            [0x5a; 32]
        };
        let key = AccountKeysHandle::for_test(account, [0; 32], modify);
        let fields = ModifyAuthorization {
            account,
            op_tag,
            amount,
            op_nonce,
            chain_id,
        };
        let mac = ledger.authorization.modify_mac(&key, &fields);
        assert_eq!(B256::from(mac), expected);
        assert!(ledger.authorization.verify_modify_auth(&key, &fields, &mac));
    }
}

#[test]
fn promis_attestation_rejects_a_gratis_domain_tag_and_a_mutated_event() {
    let sk = SigningKey::from_bytes(&[7u8; 32]);
    let pk = sk.verifying_key().to_bytes();
    let hash = B256::repeat_byte(0xAB);
    let mut promis = crate::protocol::PromisOpResult {
        status: PromisOpStatus::Applied,
        new_balance: vec![1, 2, 3],
        event_amount: U256::from(3u64),
        encrypted_event_amount: vec![4, 5, 6, 7],
        next_op_nonce: 4,
        inputs_canonical_hash: hash,
        attestation_tag: Vec::new(),
    };
    let gratis_preimage = {
        let mut bytes = b"outbe/tee/gratis-attestation/v1".to_vec();
        bytes.extend_from_slice(hash.as_slice());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes
    };
    let promis_tag = sk
        .sign(&promis_op_attestation_preimage(hash, &promis).unwrap())
        .to_bytes();
    let gratis_tag = sk.sign(&gratis_preimage).to_bytes();
    verify_promis_op_attestation(&pk, hash, &promis, &promis_tag).unwrap();
    let cross = verify_promis_op_attestation(&pk, hash, &promis, &gratis_tag).unwrap_err();
    assert_eq!(cross.metric_class(), "promis_op_attestation");
    assert!(matches!(
        &cross,
        outbe_tee::TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::SignatureMismatch(_),
        } if *descriptor == PROMIS_OP_ATTESTATION
    ));
    let mut malformed = [0u8; 32];
    malformed[0] = 2;
    assert!(VerifyingKey::from_bytes(&malformed).is_err());
    let bad_key = verify_promis_op_attestation(&malformed, hash, &promis, &[0u8; 1]).unwrap_err();
    assert!(matches!(
        &bad_key,
        outbe_tee::TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::InvalidPublicKey(_),
        } if *descriptor == PROMIS_OP_ATTESTATION
    ));
    let bad_len = verify_promis_op_attestation(&pk, hash, &promis, &[0u8; 1]).unwrap_err();
    assert!(matches!(
        &bad_len,
        outbe_tee::TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::InvalidSignatureLength { actual: 1 },
        } if *descriptor == PROMIS_OP_ATTESTATION
    ));
    promis.encrypted_event_amount[0] ^= 0xff;
    assert!(verify_promis_op_attestation(&pk, hash, &promis, &promis_tag).is_err());
}

fn sample_op(draw: u64) -> PromisOp {
    if draw.is_multiple_of(2) {
        PromisOp::Mint
    } else {
        PromisOp::Burn
    }
}

fn sample_amount(draw: u64) -> U256 {
    match draw % 5 {
        0 => U256::ZERO,
        1 => U256::MAX,
        2 => U256::from(u64::MAX),
        _ => U256::from((draw % 97) + 1),
    }
}

/// SplitMix64. The golden gamma is odd, so the low bit is not stuck the way an
/// even multiply left it.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut mixed = *state;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^ (mixed >> 31)
}

fn generated_sequences(mut state: u64, cases: u64, len_modulus: u64) -> Vec<Vec<(PromisOp, U256)>> {
    let mut sequences = Vec::with_capacity(cases as usize);
    for case in 0..cases {
        let len = (case % len_modulus) as usize;
        let mut steps = Vec::with_capacity(len);
        for _ in 0..len {
            let draw = splitmix64(&mut state);
            steps.push((sample_op(draw), sample_amount(draw >> 8)));
        }
        sequences.push(steps);
    }
    sequences
}

fn saw_repeated_ops(sequences: &[Vec<(PromisOp, U256)>]) -> (bool, bool) {
    let mut mint_mint = false;
    let mut burn_burn = false;
    for steps in sequences {
        for pair in steps.windows(2) {
            match pair {
                [(PromisOp::Mint, _), (PromisOp::Mint, _)] => mint_mint = true,
                [(PromisOp::Burn, _), (PromisOp::Burn, _)] => burn_burn = true,
                _ => {}
            }
        }
    }
    (mint_mint, burn_burn)
}

fn seed_state(seed: B256) -> u64 {
    let mut state = 0u64;
    for chunk in seed.0.chunks(8) {
        let mut bytes = [0u8; 8];
        bytes[..chunk.len()].copy_from_slice(chunk);
        state ^= u64::from_be_bytes(bytes);
    }
    state
}

struct Books {
    balance: U256,
    nonce: u64,
    blob: Vec<u8>,
}

fn conserve(steps: &[(PromisOp, U256)], label: &str) {
    let sk = state_key();
    let view = derive_view_key(&sk, alice()).unwrap();
    let mut books = Books {
        balance: U256::ZERO,
        nonce: 0,
        blob: Vec::new(),
    };
    for (op, amount) in steps {
        let expect_apply = match op {
            PromisOp::Mint => !amount.is_zero() && books.balance.checked_add(*amount).is_some(),
            PromisOp::Burn => !amount.is_zero() && books.balance >= *amount,
        };
        let mut req = request(*op, alice(), *amount, books.nonce);
        req.current_balance = books.blob.clone();
        if !amount.is_zero() {
            req.modify_auth = auth(&sk, alice(), *op, *amount, books.nonce);
        }
        let before_nonce = books.nonce;
        let before_blob = books.blob.clone();
        let result = apply_op(&sk, &req);
        assert_eq!(
            matches!(result.status, PromisOpStatus::Applied),
            expect_apply,
            "{label}"
        );
        match result.status {
            PromisOpStatus::Applied => {
                assert!(!result.new_balance.is_empty(), "{label}");
                assert_eq!(result.next_op_nonce, before_nonce + 1, "{label}");
                books.nonce = result.next_op_nonce;
                books.balance = match op {
                    PromisOp::Mint => books.balance + *amount,
                    PromisOp::Burn => books.balance - *amount,
                };
                books.blob = result.new_balance;
            }
            PromisOpStatus::Rejected { .. } => {
                assert!(result.new_balance.is_empty(), "{label}");
                assert_eq!(result.next_op_nonce, 0, "{label}");
                assert_eq!(books.nonce, before_nonce, "{label}");
                assert_eq!(books.blob, before_blob, "{label}");
            }
        }
        assert_eq!(
            decrypt_balance(&view, alice(), &books.blob).unwrap(),
            books.balance,
            "{label}"
        );
    }
}

fn conserve_seed(seed: B256) {
    let sequences = generated_sequences(seed_state(seed), 64, 33);
    assert_eq!(sequences.len(), 64, "seed {seed}");
    assert!(
        sequences.iter().any(|steps| steps.is_empty()),
        "seed {seed}"
    );
    assert!(
        sequences.iter().any(|steps| steps.len() == 32),
        "seed {seed}"
    );
    assert!(
        sequences.iter().all(|steps| steps.len() <= 32),
        "seed {seed}"
    );
    let (mint_mint, burn_burn) = saw_repeated_ops(&sequences);
    assert!(mint_mint, "seed {seed}");
    assert!(burn_burn, "seed {seed}");
    for (index, steps) in sequences.iter().enumerate() {
        conserve(
            steps,
            &format!("seed {seed} sequence {index} len {}", steps.len()),
        );
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn promis_sequences_shrink_and_conserve(
        steps in proptest::collection::vec((promis_op_strategy(), promis_amount_strategy()), 0..32)
    ) {
        conserve(&steps, "proptest");
    }
}

fn promis_op_strategy() -> impl proptest::strategy::Strategy<Value = PromisOp> {
    use proptest::prelude::Just;
    use proptest::prop_oneof;
    prop_oneof![Just(PromisOp::Mint), Just(PromisOp::Burn)]
}

fn promis_amount_strategy() -> impl proptest::strategy::Strategy<Value = U256> {
    use proptest::prelude::{any, Just};
    use proptest::prop_oneof;
    use proptest::strategy::Strategy;
    prop_oneof![
        Just(U256::ZERO),
        Just(U256::from(1u64)),
        (0u64..=1_000u64).prop_map(U256::from),
        any::<u64>().prop_map(U256::from),
        Just(U256::MAX - U256::from(1u64)),
        Just(U256::MAX),
    ]
}

#[test]
fn random_promis_ledgers_conserve_value() {
    conserve_seed(B256::repeat_byte(0x42));
    conserve_seed(B256::random());
}
