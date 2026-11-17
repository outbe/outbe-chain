//! Cohort contract and historical golden-vector tests.
use super::*;
use outbe_fidelity_math::{MAX_LEAGUE, MIN_LEAGUE};

const CHAIN: B256 = B256::repeat_byte(0xC2);
use outbe_system_common::time::SECONDS_PER_DAY as DAY;

fn state_key() -> StateKeyHandle {
    derive_fidelity_state_key(
        &crate::dev::offer_material(b"a-group-threshold-signature-~48-bytes-long!!", CHAIN),
        CHAIN,
        0,
    )
    .unwrap()
}
fn owner_view_key() -> [u8; 32] {
    let state = crate::crypto::hkdf_sha256(
        CHAIN.as_slice(),
        b"a-group-threshold-signature-~48-bytes-long!!",
        b"outbe/fidelity/state-key/v1/0",
    )
    .unwrap();
    crate::crypto::hkdf_sha256(&state, alice().as_slice(), b"outbe/fidelity/view-key/v1").unwrap()
}
fn alice() -> Address {
    Address::repeat_byte(0x11)
}
fn section(op: FidelityCohortOp, timestamp: u64, first: u64, blob: Vec<u8>) -> FidelityOpSection {
    FidelityOpSection {
        op,
        timestamp,
        first_qualified_start: first,
        current_blob: blob,
    }
}

/// Known-answer vectors pinning the FIDELITY domain byte layout: the state
/// key derivation, one cohort blob, and the query-auth preimage hash. Each of
/// these changes would make persisted on-chain cohort ciphertext undecryptable
/// or split query auth between host and enclave:
///
/// - any change to an HKDF label
/// - any change to the interior/padding layout
/// - any change to the auth message
///
/// Regenerate only on an intentional, reviewed format change.
const FIRST_DAY: u64 = outbe_system_common::time::utc_day_from_timestamp(1_000_000);

#[test]
fn divergent_cohort_transitions_rederive_independent_keys() {
    let key = state_key();
    let initial = section(FidelityCohortOp::In, 1_000_000, 0, Vec::new());
    let first = apply_cohort_section(&key, alice(), U256::from(1000), &initial).unwrap();
    let base = section(
        FidelityCohortOp::In,
        1_000_000 + DAY,
        FIRST_DAY,
        first.new_blob,
    );
    let one = apply_cohort_section(&key, alice(), U256::from(500), &base).unwrap();
    let amount = apply_cohort_section(&key, alice(), U256::from(501), &base).unwrap();
    let mut other_time = base.clone();
    other_time.timestamp += DAY;
    let time = apply_cohort_section(&key, alice(), U256::from(500), &other_time).unwrap();
    assert_eq!(
        one,
        apply_cohort_section(&key, alice(), U256::from(500), &base).unwrap()
    );
    assert_ne!(&one.new_blob[12..44], &amount.new_blob[12..44]);
    assert_ne!(&one.new_blob[12..44], &time.new_blob[12..44]);
    let view_key = FIDELITY
        .account_keys
        .derive_view_key(&key, alice())
        .unwrap();
    let (_, state) = read_state(&view_key, alice(), &one.new_blob).unwrap();
    let plaintext = crate::fidelity_decrypt::decrypt_fidelity_cohorts(
        &owner_view_key(),
        alice(),
        &one.new_blob,
    )
    .unwrap();
    assert_eq!(CohortState::decode(&plaintext).unwrap(), state);
    assert!(
        crate::fidelity_decrypt::decrypt_fidelity_cohorts(&[9; 32], alice(), &one.new_blob)
            .is_err()
    );
    assert!(crate::fidelity_decrypt::decrypt_fidelity_cohorts(
        &owner_view_key(),
        Address::repeat_byte(9),
        &one.new_blob
    )
    .is_err());
    let standalone = apply_cohort_op(
        &key,
        &FidelityCohortRequest {
            chain_id: CHAIN,
            account: alice(),
            amount: U256::from(500),
            section: base,
        },
    )
    .unwrap();
    assert_eq!(standalone.outcome, one);
}

#[test]
fn fidelity_known_answer_vectors() {
    let sk = state_key();

    let mut state = CohortState::default();
    assert_eq!(
        state.cohort_in(U256::from(1_000u64), 1_000_000),
        Some(1_000_000)
    );
    state.cohort_out(U256::from(400u64), 1_000_000 + 30 * DAY);
    let vk = FIDELITY.account_keys.derive_view_key(&sk, alice()).unwrap();
    let blob = fidelity_cipher::write_blob(
        &vk,
        CohortTransition {
            account: alice(),
            previous_version: 0,
            previous_blob: &[],
            padded_state: &state.encode_padded().unwrap(),
        },
    )
    .unwrap();
    assert_eq!(
        crate::fidelity_decrypt::decrypt_fidelity_cohorts(&owner_view_key(), alice(), &blob)
            .unwrap(),
        state.encode_padded().unwrap(),
    );
    let blob_hex = hex::encode(&blob);

    let auth_hash = hex::encode(eip191_hash(&fidelity_query_auth_message(
        CHAIN,
        alice(),
        2_000_000,
    )));

    // To regenerate: `cargo test -p outbe-enclave fidelity_known_answer -- --nocapture`
    // with the asserts commented out, then paste.
    assert_eq!(
        sk.compute_test_mac(b"kat", b"state"),
        StateKeyHandle::for_test(
            hex::decode("445d55ec5e634dcf92522bc31a29b3e6f83696ab70796158b42c06f6dac47b62")
                .unwrap()
                .try_into()
                .unwrap()
        )
        .compute_test_mac(b"kat", b"state")
    );
    assert_eq!(
        blob_hex,
        concat!(
            "0000000000000001464944324381fe9cc2c0e087ed60b82c37cbdb1821dfe17c",
            "4f3a1570875a76ce753bf1f21be9c3d7e3a1fc51146b751746db0616605727fa",
            "627bbea54b8141601b8c25eb9dde9cbabf9911bc890758648941811236274740",
            "5e892991753017e6088bab46fa8951deec9f0efe8a0e27329e41d1ed5d1ed809",
            "111ebe93bca28cc66257a3a4a2ed26ba94382f04ff69a05adcee47ee3f071388",
            "f806d4ce2825264591dbe06c20c4fcd7075f32e0532e2dbf2c326208a451b7c1",
            "3d2b16aeb10fbdf5c3087611d03a557854c6679ead00e6742a2381ca5b5557a9",
            "2256017d56d9f999452188bd4b575ca25f7b7b186c8f1d8b207603e106e915bf",
            "c97f7d85055e35adddd0abd297c18f06ab046e22b508bd0054d893d90197c8f3",
            "7d5c71f4d4138da42e35cd43f7b17146d2cf1b7c5a46087606370ef2fa9c3fc1",
            "57f2b7204d2bb2371ccc922f107d21e6e1c457debc4059ebc47f4e4fc1fa1ca7",
            "9e8c6e200487e566b84fff95e90109bcc82e1cc0b447ebf793b2380be0337100",
            "058a032f38f9fa7b64f96b7717c5173873569aa1ae839db34e83be1d1f4186fc",
            "245193025d420632ec3052c395ce72f14d615c293f754b563623de06524d9350",
            "41013754cc51e5ada1e91047",
        )
    );
    assert_eq!(
        auth_hash,
        "ec33700aff11e6138f687c7b5c76c10df507673f9a56a4d3e4676d171166fe45"
    );
}

#[test]
fn blob_roundtrip_and_padding_bucket() {
    let mut state = CohortState::default();
    state.cohort_in(U256::from(500u64), 100);
    state.cohort_in(U256::from(700u64), 200);
    state.cohort_out(U256::from(600u64), 300);
    let encoded = state.encode_padded().unwrap();
    // 3 active+sold cohorts -> bucket 8.
    assert_eq!(encoded.len(), HEADER_LEN + 8 * SOLD_ENTRY_LEN);
    assert_eq!(CohortState::decode(&encoded).unwrap(), state);

    // 9 combined -> bucket 16.
    for i in 0..7 {
        state.cohort_in(U256::from(1u64), 400 + i);
    }
    let bigger = state.encode_padded().unwrap();
    assert_eq!(bigger.len(), HEADER_LEN + 16 * SOLD_ENTRY_LEN);
    assert_eq!(CohortState::decode(&bigger).unwrap(), state);
}

#[test]
fn lifo_split_preserves_original_acquired_at() {
    let mut state = CohortState::default();
    state.cohort_in(U256::from(1_000u64), 100);
    state.cohort_in(U256::from(500u64), 200);
    // Consume 700: full-consume the youngest (500 @200), split 200 off the
    // older (1000 @100). The sold slice keeps acquired_at 100.
    state.cohort_out(U256::from(700u64), 300);
    assert_eq!(state.active, vec![(U256::from(800u64), 100)]);
    assert_eq!(
        state.sold,
        vec![
            (U256::from(500u64), 200, 300),
            (U256::from(200u64), 100, 300)
        ]
    );
    // Over-consume clamps at an empty stack, mirroring the on-chain guard.
    state.cohort_out(U256::from(10_000u64), 400);
    assert!(state.active.is_empty());
    assert_eq!(state.sold.len(), 3);
}

#[test]
fn same_utc_day_cohorts_coalesce_and_same_day_sale_annihilates() {
    let mut state = CohortState::default();
    state.cohort_in(U256::from(400u64), 100);
    state.cohort_in(U256::from(600u64), 100);
    assert_eq!(state.active, vec![(U256::from(1_000u64), 100)]);

    // Same-day acquisition and partial sale on day 150 annihilates without
    // polluting `sold`.
    state.cohort_in(U256::from(500u64), 150);
    state.cohort_out(U256::from(300u64), 150);
    assert_eq!(
        state.active,
        vec![(U256::from(1_000u64), 100), (U256::from(200u64), 150)]
    );
    assert!(state.sold.is_empty());
}

#[test]
fn evaluate_matches_direct_accumulator() {
    let mut state = CohortState::default();
    state.cohort_in(U256::from(1_000u64), FIRST_DAY);
    state.cohort_in(U256::from(500u64), FIRST_DAY + 100);
    state.cohort_out(U256::from(700u64), FIRST_DAY + 200);
    let now_day = FIRST_DAY + 400;

    let mut acc = RcfiAccumulator::default();
    for (s, a) in &state.active {
        acc.add_active(*s, *a, now_day).unwrap();
    }
    for (s, a, so) in &state.sold {
        acc.add_sold(*s, *a, *so, now_day).unwrap();
    }
    let (rcfi, eff, _) = acc.finish(state.qualified_start, now_day).unwrap();
    let max = t_dec(now_day - FIRST_DAY);
    let expected_league = league_from_rcfi(rcfi, max).unwrap();

    let (got_rcfi, got_eff, got_league) = state.evaluate(now_day, FIRST_DAY).unwrap();
    assert_eq!(got_rcfi, rcfi);
    assert_eq!(got_eff, eff);
    assert_eq!(got_league, expected_league);
    assert!((MIN_LEAGUE..=MAX_LEAGUE).contains(&got_league));
}

#[test]
fn apply_section_roundtrips_through_encryption() {
    let sk = state_key();
    let s_in = section(FidelityCohortOp::In, 1_000_000, 0, Vec::new());
    let out_in = apply_cohort_section(&sk, alice(), U256::from(1_000u64), &s_in).unwrap();
    assert_eq!(out_in.qualified_start_initialized, Some(FIRST_DAY));
    assert!(!out_in.new_blob.is_empty());
    // Global anchor was just set by this op -> league floor.
    assert_eq!(out_in.league, MIN_LEAGUE);

    // Second acquisition: existing qualified_start, global anchor known.
    let s_in2 = section(
        FidelityCohortOp::In,
        1_000_000 + 100 * DAY,
        FIRST_DAY,
        out_in.new_blob.clone(),
    );
    let out_in2 = apply_cohort_section(&sk, alice(), U256::from(500u64), &s_in2).unwrap();
    assert_eq!(out_in2.qualified_start_initialized, None);
    // Sole holder, no sales -> top league.
    assert_eq!(out_in2.league, MAX_LEAGUE);

    // Probe: no rewrite, same league as the state it probed.
    let s_probe = section(
        FidelityCohortOp::Probe,
        1_000_000 + 100 * DAY,
        FIRST_DAY,
        out_in2.new_blob.clone(),
    );
    let probe = apply_cohort_section(&sk, alice(), U256::ZERO, &s_probe).unwrap();
    assert!(probe.new_blob.is_empty());
    assert_eq!(probe.league, MAX_LEAGUE);

    // Sale drops efficiency below 1 -> league falls under the ceiling.
    let s_out = section(
        FidelityCohortOp::Out,
        1_000_000 + 200 * DAY,
        FIRST_DAY,
        out_in2.new_blob.clone(),
    );
    let out_out = apply_cohort_section(&sk, alice(), U256::from(1_200u64), &s_out).unwrap();
    assert!(out_out.league < MAX_LEAGUE);

    // Determinism: identical inputs -> identical outcome (byte-identical blob).
    let again = apply_cohort_section(&sk, alice(), U256::from(1_200u64), &s_out).unwrap();
    assert_eq!(again, out_out);
}

#[test]
fn snapshot_orders_and_defaults() {
    let sk = state_key();
    let bob = Address::repeat_byte(0x22);
    let s_in = section(FidelityCohortOp::In, 1_000_000, 0, Vec::new());
    let minted = apply_cohort_section(&sk, alice(), U256::from(1_000u64), &s_in).unwrap();

    let req = FidelitySnapshotRequest {
        timestamp: 1_000_000 + 50 * DAY,
        first_qualified_start: FIRST_DAY,
        entries: vec![
            crate::protocol::FidelitySnapshotEntry {
                owner: alice(),
                cohort_blob: minted.new_blob,
            },
            // Bob has no cohort state. He gets the league floor, not an error.
            crate::protocol::FidelitySnapshotEntry {
                owner: bob,
                cohort_blob: Vec::new(),
            },
        ],
    };
    let leagues = snapshot_leagues(&sk, &req).unwrap();
    assert_eq!(leagues.len(), 2);
    assert_eq!(leagues[0].owner, alice());
    assert_eq!(leagues[0].league, MAX_LEAGUE);
    assert_eq!(leagues[1].owner, bob);
    assert_eq!(leagues[1].league, MIN_LEAGUE);

    // Tampered ciphertext fails the whole batch, loudly.
    let mut bad = req.clone();
    if let Some(b) = bad.entries[0].cohort_blob.last_mut() {
        *b ^= 1;
    }
    assert!(snapshot_leagues(&sk, &bad).is_err());
}

/// 1e18-scaled fixed point -> f64 (via micro-units to avoid precision loss).
fn fp_to_f64(fp: U256) -> f64 {
    let micros: u128 = (fp / U256::from(1_000_000_000_000u128)).to::<u128>();
    micros as f64 / 1_000_000.0
}

/// Golden replay of the PDF `reference/decay.py` scenario through the enclave
/// `CohortState` port. The on-chain math moved here, so this test pins the
/// float-model agreement (+/-1 decayed day, +/-1e-3 efficiency). The fixture
/// lives in the fidelity crate (regenerated from `decay.py`). We read it
/// across the workspace rather than duplicate the generated artifact.
#[test]
fn golden_matches_decay_py_reference() {
    let raw = include_str!("../../../fidelity/tests/fixtures/rcfi_golden.json");
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    let txs: Vec<(u64, bool, U256)> = v["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["ts"].as_u64().unwrap(),
                t["kind"].as_str().unwrap() == "deposit",
                t["amount_units"].as_str().unwrap().parse::<U256>().unwrap(),
            )
        })
        .collect();
    let samples = v["samples"].as_array().unwrap();
    assert!(!samples.is_empty());

    for s in samples {
        let ts = s["ts"].as_u64().unwrap();
        let sample_day = outbe_system_common::time::utc_day_from_timestamp(ts);
        let want_rcfi = s["rcfi"].as_f64().unwrap();
        let want_eff = s["efficiency"].as_f64().unwrap();
        let want_dage = s["d_age"].as_f64().unwrap();

        // Rebuild state from every tx up to and including the sample instant,
        // mirroring the reference's `tx.date <= current_date` loop.
        let mut state = CohortState::default();
        for (t_ts, deposit, amount) in &txs {
            if *t_ts <= ts {
                let tx_day = outbe_system_common::time::utc_day_from_timestamp(*t_ts);
                if *deposit {
                    state.cohort_in(*amount, tx_day);
                } else {
                    state.cohort_out(*amount, tx_day);
                }
            }
        }
        let (rcfi_fp, eff_fp, dage_fp) = state.rcfi_triple(sample_day).unwrap();
        assert!(
            (fp_to_f64(rcfi_fp) - want_rcfi).abs() <= 1.0,
            "rcfi at ts={ts}: got {}, want {want_rcfi}",
            fp_to_f64(rcfi_fp)
        );
        assert!(
            (fp_to_f64(eff_fp) - want_eff).abs() <= 1e-3,
            "efficiency at ts={ts}: got {}, want {want_eff}",
            fp_to_f64(eff_fp)
        );
        assert!(
            (fp_to_f64(dage_fp) - want_dage).abs() <= 1.0,
            "d_age at ts={ts}: got {}, want {want_dage}",
            fp_to_f64(dage_fp)
        );
    }
}

/// Deterministic secp256k1 signer and its EVM address (mirrors the
/// transport tests' helper).
fn evm_signer(seed: u8) -> (k256::ecdsa::SigningKey, Address) {
    let sk = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap();
    let point = sk.verifying_key().to_encoded_point(false);
    let addr = Address::from_slice(&alloy_primitives::keccak256(&point.as_bytes()[1..])[12..]);
    (sk, addr)
}

/// EIP-191 owner signature over the query-auth message for `chain_id`.
fn query_sig(
    signer: &k256::ecdsa::SigningKey,
    chain_id: B256,
    account: Address,
    expiry: u64,
) -> Vec<u8> {
    use k256::ecdsa::signature::hazmat::PrehashSigner;
    let prehash = eip191_hash(&fidelity_query_auth_message(chain_id, account, expiry));
    let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        signer.sign_prehash(prehash.as_slice()).unwrap();
    let mut sig65 = [0u8; 65];
    sig65[..64].copy_from_slice(sig.to_bytes().as_slice());
    sig65[64] = recid.to_byte();
    sig65.to_vec()
}

fn query_req(chain_id: B256, account: Address, blob: Vec<u8>) -> FidelityQueryRequest {
    FidelityQueryRequest {
        chain_id,
        account,
        cohort_blob: blob,
        query_timestamp: 1_000_000 + 100 * DAY,
        block_timestamp: 1_500_000,
        first_qualified_start: FIRST_DAY,
        expiry: 2_000_000,
        owner_sig: Vec::new(),
    }
}

#[test]
fn query_happy_path_matches_snapshot_evaluation() {
    let sk = state_key();
    let (signer, account) = evm_signer(0x33);
    let minted = apply_cohort_section(
        &sk,
        account,
        U256::from(1_000u64),
        &section(FidelityCohortOp::In, 1_000_000, 0, Vec::new()),
    )
    .unwrap();

    let sig = query_sig(&signer, CHAIN, account, 2_000_000);
    let mut req = query_req(CHAIN, account, minted.new_blob);
    req.owner_sig = sig;
    let out = query_index(&sk, CHAIN, &req).unwrap();
    // Sole holder, no sales -> top league, and rcfi > 0.
    assert_eq!(out.league, MAX_LEAGUE);
    assert!(out.rcfi > U256::ZERO);
}

#[test]
fn query_rejects_foreign_chain_signature() {
    // The core regression: a signature made for a DIFFERENT chain (same
    // reused EOA) must not authorize a read on this enclave's chain. This
    // holds even though the host sets req.chain_id to the foreign value.
    let sk = state_key();
    let (signer, account) = evm_signer(0x44);
    let minted = apply_cohort_section(
        &sk,
        account,
        U256::from(1_000u64),
        &section(FidelityCohortOp::In, 1_000_000, 0, Vec::new()),
    )
    .unwrap();

    let foreign_chain = B256::repeat_byte(0xEE);
    let sig = query_sig(&signer, foreign_chain, account, 2_000_000);
    // Host forwards the foreign chain_id it matches the signature to.
    let mut req = query_req(foreign_chain, account, minted.new_blob);
    req.owner_sig = sig;
    // Resident chain is CHAIN, not foreign_chain -> rejected before decrypt.
    assert!(query_index(&sk, CHAIN, &req).is_err());
}

#[test]
fn query_rejects_wrong_signer_expiry_and_bad_length() {
    let sk = state_key();
    let (signer, account) = evm_signer(0x55);
    let (other, _) = evm_signer(0x56);
    let minted = apply_cohort_section(
        &sk,
        account,
        U256::from(1_000u64),
        &section(FidelityCohortOp::In, 1_000_000, 0, Vec::new()),
    )
    .unwrap();

    // Wrong signer (other's key over account's message).
    let bad_signer = query_sig(&other, CHAIN, account, 2_000_000);
    let mut req = query_req(CHAIN, account, minted.new_blob.clone());
    req.owner_sig = bad_signer;
    assert!(query_index(&sk, CHAIN, &req).is_err());

    // Expired: expiry < block_timestamp.
    let good = query_sig(&signer, CHAIN, account, 1_000);
    let mut req = query_req(CHAIN, account, minted.new_blob.clone());
    req.owner_sig = good;
    req.expiry = 1_000;
    assert!(query_index(&sk, CHAIN, &req).is_err());

    // Malformed signature length.
    let mut req = query_req(CHAIN, account, minted.new_blob);
    req.owner_sig = vec![0u8; 10];
    assert!(query_index(&sk, CHAIN, &req).is_err());
}
