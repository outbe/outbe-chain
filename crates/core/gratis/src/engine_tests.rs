//! Known-answer and conservation checks for the Gratis engine.
//!
//! The deterministic corpus stays. A proptest draws 64 sequences of length
//! `0..32` and shrinks failures. A seeded test checks 64 sequences of length
//! `0..=32`.

use alloy_primitives::{hex, Address, B256, U256, U512};
use outbe_core_common::confidential_tee::{
    decode_tagged_payload, encode_tagged_payload, hkdf_sha256, BalanceTransition,
    WitnessResponseTag, FIELD_BALANCE,
};
use outbe_fidelity::{FidelityCohortOp, FidelityOpSection};
use outbe_tee::resident::{AccountKeysHandle, ResidentOfferMaterial, StateKeyHandle};

use crate::cipher;
use crate::decrypt::decrypt_gratis_balance;
use crate::engine::{
    apply_op, decrypt_balance, decrypt_pledged, derive_account_keys, derive_gratis_state_key,
    derive_view_key, empty_auth, modify_mac, ModifyOperation, FIELD_PLEDGED,
};
use crate::protocol::{gratis_op_canonical_hash, GratisOp, GratisOpRequest, GratisOpStatus};

const CHAIN: B256 = B256::repeat_byte(0xC1);
const GROUP: &[u8] = b"a-group-threshold-signature-~48-bytes-long!!";

fn material(sig: &[u8]) -> ResidentOfferMaterial {
    ResidentOfferMaterial::for_test(sig.to_vec(), CHAIN, 0, 0).expect("offer material")
}

fn state_key() -> StateKeyHandle {
    derive_gratis_state_key(&material(GROUP), CHAIN, 0).unwrap()
}

fn alice() -> Address {
    Address::repeat_byte(0x11)
}

fn auth(
    sk: &StateKeyHandle,
    op: GratisOp,
    amount: U256,
    nonce: u64,
) -> crate::protocol::ModifyAuth {
    let keys = derive_account_keys(sk, alice()).unwrap();
    crate::protocol::ModifyAuth {
        mac: modify_mac(
            &keys,
            &ModifyOperation {
                account: alice(),
                op,
                amount,
                op_nonce: nonce,
                chain_id: CHAIN,
            },
        ),
        op_nonce: nonce,
    }
}

fn request(op: GratisOp, amount: U256, nonce: u64) -> GratisOpRequest {
    GratisOpRequest {
        op,
        chain_id: CHAIN,
        account: alice(),
        amount,
        current_balance: Vec::new(),
        current_pledged: Vec::new(),
        modify_auth: empty_auth(nonce),
        fidelity: None,
    }
}

fn test_amount(key: &StateKeyHandle, field: u8, amount: u64) -> Vec<u8> {
    cipher::write_amount(
        key,
        BalanceTransition {
            account: alice(),
            field,
            previous_version: 0,
            amount: U256::from(amount),
            input_hash: B256::ZERO,
        },
    )
    .unwrap()
}

fn pledged_blob(sk: &StateKeyHandle, amount: u64) -> Vec<u8> {
    let view = derive_view_key(sk, alice()).unwrap();
    test_amount(&view, FIELD_PLEDGED, amount)
}

fn rejected(status: &GratisOpStatus) -> bool {
    matches!(status, GratisOpStatus::Rejected { .. })
}

#[test]
fn gratis_known_answer_vectors() {
    const KAT_GROUP_SIG: &[u8] = b"kat-fixed-gratis-group-signature-48b-padding";
    let sk = derive_gratis_state_key(&material(KAT_GROUP_SIG), CHAIN, 0).unwrap();
    let expected_state = StateKeyHandle::for_test(hex!(
        "ee0bcead11e31dbafbf16c5b7fb2aa659045c38a7259db9002aa66bc9d9b08b3"
    ));
    assert_eq!(
        sk.compute_test_mac(b"kat", b"state"),
        expected_state.compute_test_mac(b"kat", b"state")
    );
    let view = derive_view_key(&sk, alice()).unwrap();
    let blob = cipher::write_amount(
        &view,
        BalanceTransition {
            account: alice(),
            field: FIELD_BALANCE,
            previous_version: 0,
            amount: U256::from(1000u64),
            input_hash: B256::ZERO,
        },
    )
    .unwrap();
    assert_eq!(
        hex::encode(&blob),
        "00000000000000014752413246232be65ee469056f27d81bd908bab2cd61705404526e19acfa103c5f924bfb368f1a38d1b119051b1b5fb22425659291417af423d4475f231b154efb71e9de9e128688bc118a0bf920c64f413581a4"
    );
    let view_bytes = hkdf_sha256(
        &hex!("ee0bcead11e31dbafbf16c5b7fb2aa659045c38a7259db9002aa66bc9d9b08b3"),
        alice().as_slice(),
        b"outbe/gratis/view-key/v1",
    )
    .unwrap();
    assert_eq!(
        decrypt_gratis_balance(&view_bytes, alice(), &blob).unwrap(),
        U256::from(1000u64)
    );
    let pledged = test_amount(&view, FIELD_PLEDGED, 1000);
    assert_eq!(
        decrypt_pledged(&view, alice(), &pledged).unwrap(),
        U256::from(1000u64)
    );
    assert!(decrypt_balance(&view, alice(), &pledged).is_err());
    let keys = derive_account_keys(&sk, alice()).unwrap();
    let mac = modify_mac(
        &keys,
        &ModifyOperation {
            account: alice(),
            op: GratisOp::Mint,
            amount: U256::from(1000u64),
            op_nonce: 0,
            chain_id: CHAIN,
        },
    );
    assert_eq!(
        hex::encode(mac),
        "688879e6e80acafeb78b7804de6edbd1032d95b2b654a049824c269c4aace152"
    );
}

#[test]
fn mine_is_deterministic_across_calls() {
    let sk = state_key();
    let mut req = request(GratisOp::Mint, U256::from(1000u64), 0);
    req.modify_auth = auth(&sk, GratisOp::Mint, req.amount, 0);
    let first = apply_op(&sk, &req);
    let second = apply_op(&sk, &req);
    assert_eq!(
        first, second,
        "same state key + request -> byte-identical result"
    );
    assert!(matches!(first.status, GratisOpStatus::Applied));
    assert!(!first.new_balance.is_empty());
}

#[test]
fn view_key_decrypts_minted_balance() {
    let sk = state_key();
    let mut req = request(GratisOp::Mint, U256::from(4242u64), 0);
    req.modify_auth = auth(&sk, GratisOp::Mint, req.amount, 0);
    let result = apply_op(&sk, &req);
    let view = derive_view_key(&sk, alice()).unwrap();
    assert_eq!(
        decrypt_balance(&view, alice(), &result.new_balance).unwrap(),
        U256::from(4242u64)
    );
    assert_eq!(result.next_op_nonce, 1);
}

#[test]
fn mine_rejects_forged_modify_auth() {
    let sk = state_key();
    let mut req = request(GratisOp::Mint, U256::from(1u64), 0);
    req.modify_auth = auth(&sk, GratisOp::Mint, req.amount, 0);
    req.modify_auth.mac[0] ^= 0xff;
    assert!(rejected(&apply_op(&sk, &req).status));
}

#[test]
fn release_pledged_moves_collateral_to_the_balance() {
    let sk = state_key();
    let view = derive_view_key(&sk, alice()).unwrap();
    let mut req = request(GratisOp::ReleasePledged, U256::from(40u64), 0);
    req.current_pledged = pledged_blob(&sk, 100);
    let result = apply_op(&sk, &req);
    assert_eq!(result.status, GratisOpStatus::Applied);
    assert_eq!(
        decrypt_pledged(&view, alice(), &result.new_pledged).unwrap(),
        U256::from(60u64)
    );
    assert_eq!(
        decrypt_balance(&view, alice(), &result.new_balance).unwrap(),
        U256::from(40u64)
    );
    assert_eq!(result.next_op_nonce, 0);
}

#[test]
fn release_pledged_rejects_more_than_pledged() {
    let sk = state_key();
    let mut req = request(GratisOp::ReleasePledged, U256::from(101u64), 0);
    req.current_pledged = pledged_blob(&sk, 100);
    assert!(rejected(&apply_op(&sk, &req).status));
}

#[test]
fn pledged_and_balance_blobs_are_not_interchangeable() {
    let sk = state_key();
    let view = derive_view_key(&sk, alice()).unwrap();
    let blob = pledged_blob(&sk, 100);
    assert!(decrypt_balance(&view, alice(), &blob).is_err());
    let mut release = request(GratisOp::ReleasePledged, U256::from(1u64), 0);
    release.current_balance = blob.clone();
    release.current_pledged = blob;
    assert!(rejected(&apply_op(&sk, &release).status));
    let balance = test_amount(&view, FIELD_BALANCE, 100);
    assert!(decrypt_pledged(&view, alice(), &balance).is_err());
    let mut burn = request(GratisOp::BurnPledged, U256::from(1u64), 0);
    burn.current_pledged = balance;
    assert!(rejected(&apply_op(&sk, &burn).status));
}

#[test]
fn pledge_moves_the_authorized_amount_into_the_pledged_balance() {
    let sk = state_key();
    let view = derive_view_key(&sk, alice()).unwrap();
    let mut mint = request(GratisOp::Mint, U256::from(100u64), 0);
    mint.modify_auth = auth(&sk, GratisOp::Mint, mint.amount, 0);
    let minted = apply_op(&sk, &mint);
    let mut pledge = request(GratisOp::Pledge, U256::from(30u64), 1);
    pledge.current_balance = minted.new_balance;
    pledge.modify_auth = auth(&sk, GratisOp::Pledge, pledge.amount, 1);
    let result = apply_op(&sk, &pledge);
    assert_eq!(result.status, GratisOpStatus::Applied);
    assert_eq!(
        decrypt_balance(&view, alice(), &result.new_balance).unwrap(),
        U256::from(70u64)
    );
    assert_eq!(
        decrypt_pledged(&view, alice(), &result.new_pledged).unwrap(),
        U256::from(30u64)
    );
    assert_eq!(result.next_op_nonce, 2);
    pledge.modify_auth = auth(&sk, GratisOp::Mint, pledge.amount, 1);
    assert!(rejected(&apply_op(&sk, &pledge).status));
}

#[test]
fn pledge_requires_sufficient_liquid_balance() {
    let sk = state_key();
    let mut pledge = request(GratisOp::Pledge, U256::from(1u64), 0);
    pledge.current_pledged = pledged_blob(&sk, 100);
    pledge.modify_auth = auth(&sk, GratisOp::Pledge, pledge.amount, 0);
    assert!(rejected(&apply_op(&sk, &pledge).status));
}

#[test]
fn burn_pledged_debits_only_the_pledged_balance() {
    let sk = state_key();
    let view = derive_view_key(&sk, alice()).unwrap();
    let mut req = request(GratisOp::BurnPledged, U256::from(40u64), 0);
    req.current_pledged = pledged_blob(&sk, 100);
    let result = apply_op(&sk, &req);
    assert_eq!(result.status, GratisOpStatus::Applied);
    assert!(result.new_balance.is_empty());
    assert_eq!(
        decrypt_pledged(&view, alice(), &result.new_pledged).unwrap(),
        U256::from(60u64)
    );
    req.current_balance = vec![0xAB; 56];
    assert_eq!(apply_op(&sk, &req).status, GratisOpStatus::Applied);
    req.amount = U256::from(101u64);
    assert!(rejected(&apply_op(&sk, &req).status));
}

#[test]
fn pledged_blobs_bind_the_inputs_hash_and_round_trip_on_tag_1() {
    let sk = state_key();
    let mut req = request(GratisOp::ReleasePledged, U256::from(1u64), 0);
    req.current_pledged = pledged_blob(&sk, 100);
    let result = apply_op(&sk, &req);
    let mut other = req.clone();
    other.current_pledged = pledged_blob(&sk, 101);
    assert_ne!(
        result.inputs_canonical_hash,
        gratis_op_canonical_hash(&other)
    );
    let encoded = postcard::to_allocvec(&req).unwrap();
    let decoded: GratisOpRequest = postcard::from_bytes(&encoded).unwrap();
    assert_eq!(decoded, req);
    let wire = encode_tagged_payload(WitnessResponseTag::GratisOpApplied, &result).unwrap();
    let decoded_result: crate::protocol::GratisOpResult =
        decode_tagged_payload(WitnessResponseTag::GratisOpApplied, &wire).unwrap();
    assert_eq!(decoded_result, result);
    assert_eq!(wire[0], WitnessResponseTag::GratisOpApplied.value());
}

#[test]
fn collateral_ops_reject_a_fidelity_section() {
    let sk = state_key();
    for op in [GratisOp::ReleasePledged, GratisOp::BurnPledged] {
        let mut req = request(op, U256::from(1u64), 0);
        req.current_pledged = pledged_blob(&sk, 100);
        req.fidelity = Some(FidelityOpSection {
            op: FidelityCohortOp::Out,
            timestamp: 1,
            first_qualified_start: 0,
            current_blob: Vec::new(),
        });
        assert!(rejected(&apply_op(&sk, &req).status));
    }
}

#[test]
fn burn_requires_sufficient_balance() {
    let sk = state_key();
    let mut mint = request(GratisOp::Mint, U256::from(100u64), 0);
    mint.modify_auth = auth(&sk, GratisOp::Mint, mint.amount, 0);
    let minted = apply_op(&sk, &mint);
    let mut burn = request(GratisOp::Burn, U256::from(200u64), 1);
    burn.current_balance = minted.new_balance;
    burn.modify_auth = auth(&sk, GratisOp::Burn, burn.amount, 1);
    assert!(rejected(&apply_op(&sk, &burn).status));
}

#[test]
fn derived_handles_match_key_vectors() {
    let sk = state_key();
    assert_eq!(
        sk.aead_encrypt(&[0; 12], b"golden").unwrap(),
        StateKeyHandle::for_test(hex!(
            "b0acf797d345d88dfebd4b12df2f1b62852e8136e54e0d8a35a001c20a7aab7c"
        ))
        .aead_encrypt(&[0; 12], b"golden")
        .unwrap()
    );
    let view = derive_view_key(&sk, alice()).unwrap();
    assert_eq!(
        view.aead_encrypt(&[0; 12], b"golden").unwrap(),
        StateKeyHandle::for_test(hex!(
            "56a2b1d12dbc33ca80a830adf878434eb278584a2e1cb87645cb42fe133f1598"
        ))
        .aead_encrypt(&[0; 12], b"golden")
        .unwrap()
    );
    let keys = derive_account_keys(&sk, alice()).unwrap();
    assert_eq!(keys.account(), alice());
    let operation = ModifyOperation {
        account: alice(),
        op: GratisOp::Mint,
        amount: U256::from(1),
        op_nonce: 0,
        chain_id: CHAIN,
    };
    assert_eq!(
        modify_mac(&keys, &operation),
        modify_mac(
            &AccountKeysHandle::for_test(
                alice(),
                [0; 32],
                hex!("2c1a89c4080e2e568dd2aa76ea4a72d0b1af8685655278b465e232ca25c8a602")
            ),
            &operation
        )
    );
}

struct Books {
    liquid: U256,
    pledged: U256,
    minted: U512,
    burned: U512,
    burned_pledged: U512,
    nonce: u64,
    balance_blob: Vec<u8>,
    pledged_blob: Vec<u8>,
}

fn should_apply(op: GratisOp, amount: U256, books: &Books) -> bool {
    if amount.is_zero() {
        return false;
    }
    match op {
        GratisOp::Mint => books.liquid.checked_add(amount).is_some(),
        GratisOp::Burn => books.liquid >= amount,
        GratisOp::Pledge => books.liquid >= amount && books.pledged.checked_add(amount).is_some(),
        GratisOp::ReleasePledged => {
            books.pledged >= amount && books.liquid.checked_add(amount).is_some()
        }
        GratisOp::BurnPledged => books.pledged >= amount,
    }
}

fn note_applied(op: GratisOp, amount: U256, books: &mut Books) {
    match op {
        GratisOp::Mint => {
            books.nonce += 1;
            books.liquid += amount;
            books.minted += U512::from(amount);
        }
        GratisOp::Burn => {
            books.nonce += 1;
            books.liquid -= amount;
            books.burned += U512::from(amount);
        }
        GratisOp::Pledge => {
            books.nonce += 1;
            books.liquid -= amount;
            books.pledged += amount;
        }
        GratisOp::ReleasePledged => {
            books.pledged -= amount;
            books.liquid += amount;
        }
        GratisOp::BurnPledged => {
            books.pledged -= amount;
            books.burned_pledged += U512::from(amount);
        }
    }
}

fn historical_steps() -> Vec<(GratisOp, U256)> {
    vec![
        (GratisOp::Mint, U256::ZERO),
        (GratisOp::Mint, U256::from(1000u64)),
        (GratisOp::Mint, U256::MAX),
        (GratisOp::Burn, U256::ZERO),
        (GratisOp::Burn, U256::from(2000u64)),
        (GratisOp::Burn, U256::from(400u64)),
        (GratisOp::Pledge, U256::ZERO),
        (GratisOp::Pledge, U256::from(700u64)),
        (GratisOp::Pledge, U256::from(100u64)),
        (GratisOp::ReleasePledged, U256::ZERO),
        (GratisOp::ReleasePledged, U256::from(101u64)),
        (GratisOp::ReleasePledged, U256::from(40u64)),
        (GratisOp::BurnPledged, U256::ZERO),
        (GratisOp::BurnPledged, U256::from(100u64)),
        (GratisOp::BurnPledged, U256::from(60u64)),
        (GratisOp::Mint, U256::from(1u64)),
        (GratisOp::Pledge, U256::MAX),
        (GratisOp::Burn, U256::MAX - U256::from(1u64)),
    ]
}

fn sample_amount(draw: u64) -> U256 {
    match draw % 8 {
        0 => U256::ZERO,
        1 => U256::from(1u64),
        2 => U256::from(draw % 1_001),
        3 => U256::from(1_000u64),
        4 => U256::MAX,
        5 => U256::MAX - U256::from(1u64),
        6 => U256::from(7u64),
        _ => U256::from(400u64),
    }
}

fn sample_op(draw: u64) -> GratisOp {
    match draw % 5 {
        0 => GratisOp::Mint,
        1 => GratisOp::Burn,
        2 => GratisOp::Pledge,
        3 => GratisOp::ReleasePledged,
        _ => GratisOp::BurnPledged,
    }
}

fn generated_sequences(mut state: u64, cases: u64, len_modulus: u64) -> Vec<Vec<(GratisOp, U256)>> {
    let mut sequences = Vec::with_capacity(cases as usize);
    for case in 0..cases {
        let len = (case % len_modulus) as usize;
        let mut steps = Vec::with_capacity(len);
        for index in 0..len {
            state = state
                .wrapping_mul(0x6c62_272e_07bb_0142)
                .wrapping_add(case)
                .wrapping_add(index as u64);
            steps.push((sample_op(state), sample_amount(state >> 8)));
        }
        sequences.push(steps);
    }
    sequences
}

fn conservation_sequences() -> Vec<Vec<(GratisOp, U256)>> {
    let mut sequences = Vec::with_capacity(64);
    sequences.push(historical_steps());
    sequences.extend(generated_sequences(0x9e37_79b9_7f4a_7c15, 63, 32));
    sequences
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

fn conserve(steps: &[(GratisOp, U256)], label: &str) {
    let sk = state_key();
    let view = derive_view_key(&sk, alice()).unwrap();
    let mut books = Books {
        liquid: U256::ZERO,
        pledged: U256::ZERO,
        minted: U512::ZERO,
        burned: U512::ZERO,
        burned_pledged: U512::ZERO,
        nonce: 0,
        balance_blob: Vec::new(),
        pledged_blob: Vec::new(),
    };
    for (op, amount) in steps {
        let expect_apply = should_apply(*op, *amount, &books);
        let mut req = request(*op, *amount, books.nonce);
        req.current_balance = books.balance_blob.clone();
        req.current_pledged = books.pledged_blob.clone();
        if matches!(op, GratisOp::Mint | GratisOp::Burn | GratisOp::Pledge) {
            req.modify_auth = auth(&sk, *op, *amount, books.nonce);
        }
        let result = apply_op(&sk, &req);
        assert_eq!(
            matches!(result.status, GratisOpStatus::Applied),
            expect_apply,
            "{label}"
        );
        match result.status {
            GratisOpStatus::Applied => {
                assert_applied_shape(*op, books.nonce, &result, label);
                note_applied(*op, *amount, &mut books);
                if !result.new_balance.is_empty() {
                    books.balance_blob = result.new_balance;
                }
                if !result.new_pledged.is_empty() {
                    books.pledged_blob = result.new_pledged;
                }
            }
            GratisOpStatus::Rejected { .. } => {
                assert!(result.new_balance.is_empty(), "{label}");
                assert!(result.new_pledged.is_empty(), "{label}");
                assert_eq!(result.next_op_nonce, 0, "{label}");
            }
        }
        let liquid = decrypt_balance(&view, alice(), &books.balance_blob).unwrap();
        let pledged = decrypt_pledged(&view, alice(), &books.pledged_blob).unwrap();
        assert_eq!(liquid, books.liquid, "{label}");
        assert_eq!(pledged, books.pledged, "{label}");
        assert_eq!(
            U512::from(liquid) + U512::from(pledged),
            books.minted - books.burned - books.burned_pledged,
            "{label}"
        );
    }
}

#[test]
fn gratis_ledgers_conserve_value() {
    let sequences = conservation_sequences();
    assert_eq!(sequences.len(), 64);
    assert!(sequences.iter().any(|steps| steps.is_empty()));
    assert!(sequences.iter().any(|steps| steps.len() == 31));
    assert!(sequences.iter().all(|steps| steps.len() < 32));
    for steps in &sequences {
        conserve(steps, "deterministic corpus");
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn gratis_sequences_shrink_and_conserve(
        steps in proptest::collection::vec((gratis_op_strategy(), gratis_amount_strategy()), 0..32)
    ) {
        conserve(&steps, "proptest");
    }
}

fn gratis_op_strategy() -> impl proptest::strategy::Strategy<Value = GratisOp> {
    use proptest::prelude::Just;
    use proptest::prop_oneof;
    prop_oneof![
        Just(GratisOp::Mint),
        Just(GratisOp::Burn),
        Just(GratisOp::Pledge),
        Just(GratisOp::ReleasePledged),
        Just(GratisOp::BurnPledged),
    ]
}

fn gratis_amount_strategy() -> impl proptest::strategy::Strategy<Value = U256> {
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
fn random_gratis_ledgers_conserve_value() {
    let seed = B256::random();
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
    for (index, steps) in sequences.iter().enumerate() {
        conserve(
            steps,
            &format!("seed {seed} sequence {index} len {}", steps.len()),
        );
    }
}

fn assert_applied_shape(
    op: GratisOp,
    nonce: u64,
    result: &crate::protocol::GratisOpResult,
    label: &str,
) {
    match op {
        GratisOp::Mint | GratisOp::Burn => {
            assert!(!result.new_balance.is_empty(), "{label}");
            assert!(result.new_pledged.is_empty(), "{label}");
            assert_eq!(result.next_op_nonce, nonce + 1, "{label}");
        }
        GratisOp::Pledge | GratisOp::ReleasePledged => {
            assert!(!result.new_balance.is_empty(), "{label}");
            assert!(!result.new_pledged.is_empty(), "{label}");
            let expected_nonce = if op == GratisOp::Pledge { nonce + 1 } else { 0 };
            assert_eq!(result.next_op_nonce, expected_nonce, "{label}");
        }
        GratisOp::BurnPledged => {
            assert!(result.new_balance.is_empty(), "{label}");
            assert!(!result.new_pledged.is_empty(), "{label}");
            assert_eq!(result.next_op_nonce, 0, "{label}");
        }
    }
}

#[test]
fn zero_account_is_rejected() {
    let sk = state_key();
    let mut req = request(GratisOp::ReleasePledged, U256::from(1u64), 0);
    req.account = Address::ZERO;
    assert!(rejected(&apply_op(&sk, &req).status));
}
