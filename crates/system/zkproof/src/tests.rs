use alloy_primitives::{Address, U256};
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonHasher};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{MAX_INPUTS, POSEIDON_GAS_BASE, POSEIDON_GAS_PER_INPUT, ZK_VERIFY_GAS};
use crate::errors::ZkProofError;
use crate::poseidon::poseidon_hash;
use crate::precompile::{dispatch_groth16, dispatch_poseidon, groth16_base_gas, poseidon_base_gas};
use crate::verify::{decode_full_proof_public_inputs, verify_full_proof, zk_verify};

const CHAIN_ID: u64 = 19_280_501;

fn fr_be(f: &Fr) -> [u8; 32] {
    let mut be = f.into_bigint().to_bytes_be();
    if be.len() < 32 {
        let pad = 32 - be.len();
        let mut padded = vec![0u8; 32];
        padded[pad..].copy_from_slice(&be);
        be = padded;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&be);
    out
}

// ---- Poseidon ------------------------------------------------------------

#[test]
fn poseidon_empty_input_errors() {
    assert!(matches!(poseidon_hash(&[]), Err(ZkProofError::EmptyInput)));
}

#[test]
fn poseidon_unaligned_input_errors() {
    assert!(matches!(
        poseidon_hash(&[0u8; 31]),
        Err(ZkProofError::UnalignedInput(31))
    ));
    assert!(matches!(
        poseidon_hash(&[0u8; 33]),
        Err(ZkProofError::UnalignedInput(33))
    ));
}

#[test]
fn poseidon_too_many_inputs_errors() {
    let buf = vec![0u8; (MAX_INPUTS + 1) * 32];
    match poseidon_hash(&buf) {
        Err(ZkProofError::TooManyInputs(n)) => assert_eq!(n, MAX_INPUTS + 1),
        other => panic!("expected TooManyInputs, got {other:?}"),
    }
}

#[test]
fn poseidon_n1_matches_offchain_reference() {
    let x = Fr::from(42u64);
    let on_chain = poseidon_hash(&fr_be(&x)).unwrap();
    let mut hasher = Poseidon::<Fr>::new_circom(1).unwrap();
    let off_chain = hasher.hash(&[x]).unwrap();
    assert_eq!(on_chain, fr_be(&off_chain));
}

#[test]
fn poseidon_n2_matches_offchain_reference() {
    let a = Fr::from(0x123456789abcdef0u64);
    let b = Fr::from(0xfedcba9876543210u64);
    let mut input = Vec::with_capacity(64);
    input.extend_from_slice(&fr_be(&a));
    input.extend_from_slice(&fr_be(&b));

    let on_chain = poseidon_hash(&input).unwrap();
    let mut hasher = Poseidon::<Fr>::new_circom(2).unwrap();
    let off_chain = hasher.hash(&[a, b]).unwrap();
    assert_eq!(on_chain, fr_be(&off_chain));
}

#[test]
fn poseidon_n4_matches_binding_hash_construction() {
    let sender = Fr::from(0x1122334455_u64);
    let tdid_lo = Fr::from(0xdeadbeef_u64);
    let tdid_hi = Fr::from(0xcafef00d_u64);
    let chainid = Fr::from(19_280_501_u64);

    let mut input = Vec::with_capacity(128);
    for f in [&sender, &tdid_lo, &tdid_hi, &chainid] {
        input.extend_from_slice(&fr_be(f));
    }

    let on_chain = poseidon_hash(&input).unwrap();
    let mut hasher = Poseidon::<Fr>::new_circom(4).unwrap();
    let off_chain = hasher.hash(&[sender, tdid_lo, tdid_hi, chainid]).unwrap();
    assert_eq!(on_chain, fr_be(&off_chain));
}

#[test]
fn poseidon_base_gas_formula() {
    assert_eq!(poseidon_base_gas(&[]), POSEIDON_GAS_BASE);
    assert_eq!(
        poseidon_base_gas(&[0u8; 32]),
        POSEIDON_GAS_BASE + POSEIDON_GAS_PER_INPUT
    );
    assert_eq!(
        poseidon_base_gas(&[0u8; 32 * 4]),
        POSEIDON_GAS_BASE + 4 * POSEIDON_GAS_PER_INPUT
    );
    assert_eq!(
        poseidon_base_gas(&[0u8; 32 * 12]),
        POSEIDON_GAS_BASE + 12 * POSEIDON_GAS_PER_INPUT
    );
}

// ---- zkVerify ------------------------------------------------------------

fn abi_encode(circuit_hash: &[u8; 32], proof: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + 32 + proof.len() + 32);
    out.extend_from_slice(circuit_hash);
    let mut offset = [0u8; 32];
    offset[24..32].copy_from_slice(&64u64.to_be_bytes());
    out.extend_from_slice(&offset);
    let mut len = [0u8; 32];
    len[24..32].copy_from_slice(&(proof.len() as u64).to_be_bytes());
    out.extend_from_slice(&len);
    out.extend_from_slice(proof);
    let pad = (32 - proof.len() % 32) % 32;
    out.extend(core::iter::repeat_n(0u8, pad));
    out
}

fn combined_full_proof(public_inputs: [[u8; 32]; 4], proof_words: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 * (4 + proof_words));
    out.extend_from_slice(&4u32.to_be_bytes());
    for public_input in public_inputs {
        out.extend_from_slice(&public_input);
    }
    out.resize(out.len() + proof_words * 32, 0);
    out
}

#[test]
fn full_proof_public_inputs_are_decoded_in_circuit_order() {
    let words = [
        fr_be(&Fr::from(11u64)),
        fr_be(&Fr::from(22u64)),
        fr_be(&Fr::from(33u64)),
        fr_be(&Fr::from(44u64)),
    ];
    let proof = combined_full_proof(words, 274);

    let decoded = decode_full_proof_public_inputs(&proof).unwrap();

    assert_eq!(decoded.derived_owner, words[0]);
    assert_eq!(decoded.nft_hash, words[1]);
    assert_eq!(decoded.binding_hash, words[2]);
    assert_eq!(decoded.merkle_root, words[3]);
}

#[test]
fn full_proof_rejects_wrong_public_input_count() {
    let mut proof = combined_full_proof([[0u8; 32]; 4], 274);
    proof[..4].copy_from_slice(&3u32.to_be_bytes());

    assert!(matches!(
        decode_full_proof_public_inputs(&proof),
        Err(ZkProofError::WrongPublicInputCount {
            expected: 4,
            actual: 3
        })
    ));
}

#[test]
fn full_proof_rejects_truncated_public_inputs() {
    let proof = [4u32.to_be_bytes().as_slice(), &[0u8; 64]].concat();

    assert!(matches!(
        decode_full_proof_public_inputs(&proof),
        Err(ZkProofError::TruncatedPublicInputs { .. })
    ));
}

#[test]
fn full_proof_rejects_non_canonical_public_input() {
    let modulus = Fr::MODULUS.to_bytes_be();
    let mut non_canonical = [0u8; 32];
    non_canonical[32 - modulus.len()..].copy_from_slice(&modulus);
    let mut words = [[0u8; 32]; 4];
    words[2] = non_canonical;
    let proof = combined_full_proof(words, 274);

    assert!(matches!(
        decode_full_proof_public_inputs(&proof),
        Err(ZkProofError::NonCanonicalPublicInput(2))
    ));
}

#[test]
fn full_proof_rejects_wrong_proof_section_length() {
    let empty = combined_full_proof([[0u8; 32]; 4], 0);
    assert!(matches!(
        decode_full_proof_public_inputs(&empty),
        Err(ZkProofError::WrongCombinedProofLength { .. })
    ));

    let oversized = combined_full_proof([[0u8; 32]; 4], 275);
    assert!(matches!(
        decode_full_proof_public_inputs(&oversized),
        Err(ZkProofError::WrongCombinedProofLength { .. })
    ));
}

#[test]
fn full_proof_with_invalid_curve_points_returns_backend_error() {
    let proof = combined_full_proof([[0u8; 32]; 4], 274);

    assert!(matches!(
        verify_full_proof(&proof),
        Err(ZkProofError::VerificationBackend(_))
    ));
}

#[test]
fn zk_verify_input_too_short_errors() {
    assert!(matches!(
        zk_verify(&[0u8; 32]),
        Err(ZkProofError::InputTooShort(32))
    ));
}

#[test]
fn zk_verify_unknown_circuit_returns_zero() {
    let buf = abi_encode(&[0u8; 32], &[0u8; 64]);
    let out = zk_verify(&buf).unwrap();
    assert_eq!(out, [0u8; 32]);
}

#[test]
fn zk_verify_truncated_payload_errors() {
    let mut buf = abi_encode(&[0xAB; 32], &[0xCD; 10]);
    buf.truncate(70);
    assert!(matches!(
        zk_verify(&buf),
        Err(ZkProofError::MalformedAbi(_))
    ));
}

/// An offset word of `u64::MAX` used to wrap `offset + 32` to `31` in a
/// release build, defeat the `input.len() < offset + 32` guard, and panic on
/// the out-of-range slice index - a permissionless halt of every validator
/// executing the `0xEE08` call.
#[test]
fn zk_verify_max_offset_is_rejected_not_panicking() {
    let mut input = [0u8; 96];
    input[56..64].copy_from_slice(&u64::MAX.to_be_bytes());
    assert!(matches!(
        zk_verify(&input),
        Err(ZkProofError::MalformedAbi("non-canonical offset"))
    ));
}

#[test]
fn zk_verify_offset_just_past_canonical_is_rejected() {
    let mut buf = abi_encode(&[0xAB; 32], &[0xCD; 32]);
    buf[56..64].copy_from_slice(&65u64.to_be_bytes());
    assert!(matches!(
        zk_verify(&buf),
        Err(ZkProofError::MalformedAbi("non-canonical offset"))
    ));
}

/// The in-bounds-but-non-canonical offset that decoded before the gate: it
/// points one word past the length slot, so the input used to decode to a
/// different proof slice than the one the encoder wrote.
#[test]
fn zk_verify_shifted_offset_is_rejected() {
    let mut buf = abi_encode(&[0xAB; 32], &[0xCD; 32]);
    buf[56..64].copy_from_slice(&96u64.to_be_bytes());
    assert!(matches!(
        zk_verify(&buf),
        Err(ZkProofError::MalformedAbi("non-canonical offset"))
    ));
}

#[test]
fn groth16_base_gas_is_flat() {
    assert_eq!(groth16_base_gas(&[]), ZK_VERIFY_GAS);
    assert_eq!(groth16_base_gas(&[0u8; 1024]), ZK_VERIFY_GAS);
}

// ---- dispatch (msg.value rejection) --------------------------------------

#[test]
fn dispatch_poseidon_rejects_nonzero_value() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    let res = dispatch_poseidon(storage, &[0u8; 32], Address::ZERO, U256::from(1u64));
    assert!(matches!(res, Err(PrecompileError::Revert(_))));
}

#[test]
fn dispatch_groth16_rejects_nonzero_value() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    let input = abi_encode(&[0u8; 32], &[0u8; 16]);
    let res = dispatch_groth16(storage, &input, Address::ZERO, U256::from(1u64));
    assert!(matches!(res, Err(PrecompileError::Revert(_))));
}

#[test]
fn dispatch_poseidon_happy_path_zero_value() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    let out = dispatch_poseidon(storage, &[0u8; 32], Address::ZERO, U256::ZERO).unwrap();
    assert_eq!(out.len(), 32);
}

#[test]
fn dispatch_groth16_unknown_circuit_returns_zero_bytes() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    let input = abi_encode(&[0u8; 32], &[0u8; 64]);
    let out = dispatch_groth16(storage, &input, Address::ZERO, U256::ZERO).unwrap();
    assert_eq!(out.as_ref(), &[0u8; 32]);
}

// ---- emit mint -----------------------------------------------------------

use crate::verify::{
    decode_emit_mint_public_inputs, verify_emit_mint, EMIT_MINT_COMBINED_LEN, EMIT_MINT_PROOF_WORDS,
};

const EMIT_MINT_FIELD_WORDS: usize = 25;

fn owner_word(byte: u8) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[31] = byte;
    word
}

fn u64_word(value: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..32].copy_from_slice(&value.to_be_bytes());
    word
}

fn valid_emit_words() -> [[u8; 32]; EMIT_MINT_FIELD_WORDS] {
    let mut words = [[0u8; 32]; EMIT_MINT_FIELD_WORDS];
    words[0] = u64_word(31_337);
    words[1] = fr_be(&Fr::from(102u64));
    words[2] = fr_be(&Fr::from(103u64));
    for word in words.iter_mut().take(23).skip(3) {
        *word = owner_word(0x22);
    }
    words[23] = u64_word(40);
    words[24] = fr_be(&Fr::from(104u64));
    words
}

fn combined_emit_proof(words: &[[u8; 32]; EMIT_MINT_FIELD_WORDS], proof_words: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 * (EMIT_MINT_FIELD_WORDS + proof_words));
    out.extend_from_slice(&(EMIT_MINT_FIELD_WORDS as u32).to_be_bytes());
    for word in words {
        out.extend_from_slice(word);
    }
    out.resize(out.len() + proof_words * 32, 0);
    out
}

#[test]
fn emit_mint_public_inputs_are_decoded_in_circuit_order() {
    let words = valid_emit_words();
    let proof = combined_emit_proof(&words, EMIT_MINT_PROOF_WORDS);

    let decoded = decode_emit_mint_public_inputs(&proof).unwrap();

    assert_eq!(decoded.chain_id, 31_337);
    assert_eq!(decoded.root, words[1]);
    assert_eq!(decoded.nullifier, words[2]);
    assert_eq!(decoded.note_owner.0.as_slice(), &[0x22; 20]);
    assert_eq!(decoded.mint_units, 40);
    assert_eq!(decoded.change_commitment, words[24]);
}

#[test]
fn emit_mint_rejects_wrong_public_input_count() {
    for count in [24u32, 26u32] {
        let mut proof = combined_emit_proof(&valid_emit_words(), EMIT_MINT_PROOF_WORDS);
        proof[..4].copy_from_slice(&count.to_be_bytes());
        assert!(matches!(
            decode_emit_mint_public_inputs(&proof),
            Err(ZkProofError::WrongPublicInputCount { actual, .. }) if actual == count as usize
        ));
    }
}

#[test]
fn emit_mint_rejects_truncated_public_inputs() {
    let proof = combined_emit_proof(&valid_emit_words(), EMIT_MINT_PROOF_WORDS);
    // The exact-length gate fires first for anything that is not the frozen
    // wire format — including blobs shorter than the public prefix.
    assert!(matches!(
        decode_emit_mint_public_inputs(&proof[..500]),
        Err(ZkProofError::WrongCombinedProofLength { actual: 500, .. })
    ));
}

#[test]
fn emit_mint_accepts_exactly_the_frozen_length() {
    let words = valid_emit_words();
    let proof = combined_emit_proof(&words, EMIT_MINT_PROOF_WORDS);
    assert_eq!(proof.len(), EMIT_MINT_COMBINED_LEN);
    assert!(decode_emit_mint_public_inputs(&proof).is_ok());
}

#[test]
fn emit_mint_rejects_any_non_frozen_length() {
    let words = valid_emit_words();
    // Empty, short, unaligned, one-word-over — every deviation from the
    // frozen transcript length is the same error.
    for tail_words in [
        0usize,
        1,
        EMIT_MINT_PROOF_WORDS - 1,
        EMIT_MINT_PROOF_WORDS + 1,
    ] {
        let proof = combined_emit_proof(&words, tail_words);
        assert!(
            matches!(
                decode_emit_mint_public_inputs(&proof),
                Err(ZkProofError::WrongCombinedProofLength { expected, actual })
                    if expected == EMIT_MINT_COMBINED_LEN && actual == proof.len()
            ),
            "tail {tail_words} words must fail the exact-length gate"
        );
    }
    let mut unaligned = combined_emit_proof(&words, EMIT_MINT_PROOF_WORDS);
    unaligned.push(0);
    assert!(matches!(
        decode_emit_mint_public_inputs(&unaligned),
        Err(ZkProofError::WrongCombinedProofLength { .. })
    ));
}

#[test]
fn emit_mint_rejects_non_canonical_field_word() {
    // The BN254 scalar modulus itself reduces to zero, so its exact
    // big-endian encoding violates the canonical-representation rule.
    let modulus_bytes: [u8; 32] =
        alloy_primitives::hex!("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001");
    for slot in [1usize, 2, 24] {
        let mut words = valid_emit_words();
        words[slot] = modulus_bytes;
        let proof = combined_emit_proof(&words, EMIT_MINT_PROOF_WORDS);
        assert!(matches!(
            decode_emit_mint_public_inputs(&proof),
            Err(ZkProofError::NonCanonicalPublicInput(index)) if index == slot
        ));
    }
}

#[test]
fn emit_mint_rejects_invalid_chain_id_word() {
    let mut words = valid_emit_words();
    words[0][0] = 1;
    let proof = combined_emit_proof(&words, EMIT_MINT_PROOF_WORDS);
    assert!(matches!(
        decode_emit_mint_public_inputs(&proof),
        Err(ZkProofError::InvalidEmitChainId)
    ));
}

#[test]
fn emit_mint_rejects_invalid_owner_byte_word() {
    let mut words = valid_emit_words();
    words[7][0] = 1;
    let proof = combined_emit_proof(&words, EMIT_MINT_PROOF_WORDS);
    assert!(matches!(
        decode_emit_mint_public_inputs(&proof),
        Err(ZkProofError::InvalidEmitOwnerByte(7))
    ));
}

#[test]
fn emit_mint_rejects_invalid_mint_units_word() {
    let mut words = valid_emit_words();
    words[23][0] = 1;
    let proof = combined_emit_proof(&words, EMIT_MINT_PROOF_WORDS);
    assert!(matches!(
        decode_emit_mint_public_inputs(&proof),
        Err(ZkProofError::InvalidEmitMintUnits)
    ));
}

#[test]
fn emit_mint_rejects_short_header() {
    assert!(matches!(
        decode_emit_mint_public_inputs(&[0u8; 3]),
        Err(ZkProofError::CombinedProofTooShort(3))
    ));
}

#[test]
fn emit_mint_combined_wrong_count_text_is_circuit_generic() {
    let mut proof = combined_emit_proof(&valid_emit_words(), 1);
    proof[..4].copy_from_slice(&24u32.to_be_bytes());
    let error = decode_emit_mint_public_inputs(&proof)
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "zk_verify: combined proof public input count is 24, expected 25"
    );
}

/// Real prove→verify round trip through the pinned Emit mint VK. The proof
/// must verify as submitted and stop verifying when any public input word is
/// changed, binding the combined wire to the frozen circuit identity.
#[test]
fn emit_mint_real_proof_verifies_and_binds_every_public_word() {
    use ark_ff::PrimeField as _;
    use outbe_protocol::primitive::hash::FieldHasher;
    use outbe_protocol::protocol::zk::ProofGenerator;
    use outbe_protocol::{OutbeV1, Suite};
    use outbe_zk_backend::barretenberg::Barretenberg;
    use outbe_zk_canonical::noir::emit_mint::{EmitMint, PublicInputs, Witness};
    use outbe_zk_canonical::CircuitId as _;

    assert_eq!(EmitMint::VERSION, "1.0.0");
    assert_eq!(
        EmitMint::CIRCUIT_HASH,
        alloy_primitives::hex!("f41d5933969dacd80409a6501cd0de2b5efa877961f20b90af0814e786eadc45")
    );
    assert_eq!(
        EmitMint::VK_HASH,
        alloy_primitives::hex!("d09623ad712f85bf032a7171bda20205620657eeb37df0c5b19a641a7295acb1")
    );

    crate::verify::init_crs().expect("CRS init");
    type Field = <OutbeV1 as Suite>::Field;

    let h2 = |left: Field, right: Field| -> Field {
        <<OutbeV1 as Suite>::Hash as FieldHasher<Field>>::hash(&[left, right]).unwrap()
    };
    let ascii = |text: &str| Field::from_be_bytes_mod_order(text.as_bytes());
    let emit_hash = |tag: &str, values: &[Field]| -> Field {
        let mut state = h2(ascii("OUTBE_EMIT"), ascii(tag));
        state = h2(state, Field::from(values.len() as u64));
        for value in values {
            state = h2(state, *value);
        }
        state
    };
    let owner = [0x22u8; 20];
    let chain_id = 31_337u64;
    let spend_key = Field::from(17u64);
    let serial = emit_hash(
        "EMIT_NOTE_SN",
        &[Field::from_be_bytes_mod_order(&owner), spend_key],
    );
    let commitment = emit_hash(
        "EMIT_COMMITMENT",
        &[Field::from(chain_id), serial, Field::from(100u64)],
    );
    let mut path = [Field::from(0u64); 20];
    path[0] = emit_hash("EMIT_EMPTY", &[Field::from(chain_id)]);
    for level in 1..20 {
        path[level] = h2(path[level - 1], path[level - 1]);
    }
    let path_bits = [false; 20];
    let mut root = commitment;
    for sibling in path {
        root = h2(root, sibling);
    }
    let nullifier = emit_hash(
        "EMIT_NULLIFIER",
        &[Field::from(chain_id), serial, spend_key],
    );
    let next_key = emit_hash("EMIT_CHANGE_KEY", &[spend_key, nullifier]);
    let change = emit_hash(
        "EMIT_COMMITMENT",
        &[
            Field::from(chain_id),
            emit_hash(
                "EMIT_NOTE_SN",
                &[Field::from_be_bytes_mod_order(&owner), next_key],
            ),
            Field::from(60u64),
        ],
    );

    let public = PublicInputs {
        chain_id,
        root,
        nullifier,
        note_owner: owner,
        mint_units: 40,
        change_commitment: change,
    };
    let witness = Witness {
        note_amount: 100,
        note_spend_key: spend_key,
        path_bits,
        auth_path: path,
    };
    let backend = Barretenberg::default();
    let proof = ProofGenerator::<OutbeV1, EmitMint>::generate(&backend, &witness, &public)
        .expect("emit mint proof generation");

    let field_word = |field: &Field| -> [u8; 32] {
        let mut word = [0u8; 32];
        let bytes = field.into_bigint().to_bytes_be();
        word[32 - bytes.len()..].copy_from_slice(&bytes);
        word
    };
    let mut combined = Vec::with_capacity(4 + 32 * (25 + proof.proof.len()));
    combined.extend_from_slice(&25u32.to_be_bytes());
    for word in <EmitMint as outbe_protocol::protocol::zk::Circuit<OutbeV1>>::public_inputs(&public)
    {
        combined.extend_from_slice(&field_word(&word));
    }
    for word in &proof.proof {
        combined.extend_from_slice(word);
    }

    assert!(
        verify_emit_mint(&combined).expect("emit verify executes"),
        "real emit mint proof must verify through the pinned VK"
    );

    // Flip exactly one public word per round: the decode stays valid (the
    // mutation keeps canonical shape) but verification must turn false.
    for slot in 0..25 {
        let mut tampered = combined.clone();
        let start = 4 + slot * 32;
        match slot {
            0 | 3..=23 => tampered[start + 31] ^= 1,
            _ => tampered[start] ^= 1,
        }
        assert!(
            !verify_emit_mint(&tampered).expect("tampered emit verify executes"),
            "mutating public word {slot} must invalidate the proof"
        );
    }
}
