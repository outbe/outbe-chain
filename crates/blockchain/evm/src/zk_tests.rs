use alloy_primitives::{Address, U256};
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonHasher};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_zk_canonical::demo_tribute::PROOF_WORDS as DEMO_TRIBUTE_PROOF_WORDS;

use crate::zk::{
    dispatch_groth16, dispatch_poseidon, groth16_base_gas, poseidon_base_gas, poseidon_hash,
    zk_verify, MAX_INPUTS, POSEIDON_GAS_BASE, POSEIDON_GAS_PER_INPUT, ZK_VERIFY_GAS,
};

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
    assert!(matches!(
        poseidon_hash(&[]),
        Err(PrecompileError::Revert(message)) if message == "poseidon: empty input"
    ));
}

#[test]
fn poseidon_unaligned_input_errors() {
    for length in [31, 33] {
        assert!(matches!(
            poseidon_hash(&vec![0u8; length]),
            Err(PrecompileError::Revert(message))
                if message == format!(
                    "poseidon: input length {length} is not a multiple of 32"
                )
        ));
    }
}

#[test]
fn poseidon_too_many_inputs_errors() {
    let buf = vec![0u8; (MAX_INPUTS + 1) * 32];
    assert!(matches!(
        poseidon_hash(&buf),
        Err(PrecompileError::Revert(message))
            if message
                == format!(
                    "poseidon: {} inputs exceeds maximum supported ({MAX_INPUTS})",
                    MAX_INPUTS + 1
                )
    ));
}

#[test]
fn poseidon_n1_matches_offchain_reference() {
    let x = Fr::from(42u64);
    let on_chain = poseidon_hash(&fr_be(&x)).unwrap();
    assert_eq!(
        on_chain,
        [
            27, 64, 141, 175, 235, 237, 223, 8, 113, 56, 131, 153, 177, 229, 59, 208, 101, 253,
            112, 241, 133, 128, 190, 92, 221, 225, 93, 126, 178, 197, 39, 67,
        ]
    );
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

fn combined_demo_tribute_proof(public_inputs: [[u8; 32]; 4], proof_words: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 * (4 + proof_words));
    out.extend_from_slice(&4u32.to_be_bytes());
    for public_input in public_inputs {
        out.extend_from_slice(&public_input);
    }
    out.resize(out.len() + proof_words * 32, 0);
    out
}

#[test]
fn demo_tribute_with_invalid_curve_points_returns_backend_error() {
    use outbe_zk_canonical::noir::demo_tribute::DemoTribute;
    use outbe_zk_canonical::CircuitId as _;

    let proof = combined_demo_tribute_proof([[0u8; 32]; 4], DEMO_TRIBUTE_PROOF_WORDS);
    let input = abi_encode(&DemoTribute::CIRCUIT_HASH, &proof);
    assert!(matches!(
        zk_verify(&input),
        Err(PrecompileError::Revert(message))
            if message.starts_with("zk verification backend failed: ")
    ));
}

#[test]
fn zk_verify_input_too_short_errors() {
    assert!(matches!(
        zk_verify(&[0u8; 32]),
        Err(PrecompileError::Revert(message))
            if message == "zk_verify: input too short (32 < 64 bytes)"
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
        Err(PrecompileError::Revert(message))
            if message == "zk_verify: malformed ABI input (offset past end)"
    ));
}

/// An offset word of `u64::MAX` used to wrap `offset + 32` to `31` in a
/// release build. Then it defeated the `input.len() < offset + 32` guard and
/// caused a panic on the out-of-range slice index. That was a permissionless
/// halt of every validator that executed the `0xEE08` call.
#[test]
fn zk_verify_max_offset_is_rejected_not_panicking() {
    let mut input = [0u8; 96];
    input[56..64].copy_from_slice(&u64::MAX.to_be_bytes());
    assert!(matches!(
        zk_verify(&input),
        Err(PrecompileError::Revert(message))
            if message == "zk_verify: malformed ABI input (non-canonical offset)"
    ));
}

#[test]
fn zk_verify_offset_just_past_canonical_is_rejected() {
    let mut buf = abi_encode(&[0xAB; 32], &[0xCD; 32]);
    buf[56..64].copy_from_slice(&65u64.to_be_bytes());
    assert!(matches!(
        zk_verify(&buf),
        Err(PrecompileError::Revert(message))
            if message == "zk_verify: malformed ABI input (non-canonical offset)"
    ));
}

/// The in-bounds-but-non-canonical offset that decoded before the gate. It
/// points one word past the length slot. Thus the input used to decode to a
/// different proof slice than the one the encoder wrote.
#[test]
fn zk_verify_shifted_offset_is_rejected() {
    let mut buf = abi_encode(&[0xAB; 32], &[0xCD; 32]);
    buf[56..64].copy_from_slice(&96u64.to_be_bytes());
    assert!(matches!(
        zk_verify(&buf),
        Err(PrecompileError::Revert(message))
            if message == "zk_verify: malformed ABI input (non-canonical offset)"
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
