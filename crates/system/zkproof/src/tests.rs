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
use crate::verify::zk_verify;

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
