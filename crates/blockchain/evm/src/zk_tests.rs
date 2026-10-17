use alloy_primitives::{Address, U256};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::zk::{dispatch_groth16, groth16_base_gas, zk_verify, ZK_VERIFY_GAS};

const CHAIN_ID: u64 = 19_280_501;

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
#[ignore = "requires the pinned Barretenberg CRS"]
fn zk_verify_real_proof_and_changed_statement() {
    use outbe_zk::noir::demo_tribute::DemoTribute;
    use outbe_zk::CircuitId as _;

    outbe_zk_backend::barretenberg::BarretenbergHandler::init().unwrap();
    let proof = include_bytes!(
        "../../../../testing/protocol-benchmarks/fixtures/tribute_demo_tribute_v1.bin"
    );
    let mut valid = [0u8; 32];
    valid[31] = 1;
    assert_eq!(
        zk_verify(&abi_encode(&DemoTribute::CIRCUIT_HASH, proof)).unwrap(),
        valid
    );

    let mut tampered = proof.to_vec();
    tampered[100..132].fill(0);
    assert_eq!(
        zk_verify(&abi_encode(&DemoTribute::CIRCUIT_HASH, &tampered)).unwrap(),
        [0u8; 32]
    );
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
fn dispatch_groth16_rejects_nonzero_value() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    let input = abi_encode(&[0u8; 32], &[0u8; 16]);
    let res = dispatch_groth16(storage, &input, Address::ZERO, U256::from(1u64));
    assert!(matches!(res, Err(PrecompileError::Revert(_))));
}

#[test]
fn dispatch_groth16_unknown_circuit_returns_zero_bytes() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    let input = abi_encode(&[0u8; 32], &[0u8; 64]);
    let out = dispatch_groth16(storage, &input, Address::ZERO, U256::ZERO).unwrap();
    assert_eq!(out.as_ref(), &[0u8; 32]);
}
