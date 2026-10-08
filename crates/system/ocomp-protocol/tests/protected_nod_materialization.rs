use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    abi::{
        decode_materialize_certified_nods_calldata,
        decode_protected_materialize_certified_nods_calldata,
        encode_protected_materialize_certified_nods_calldata,
    },
    common::BoundedBytes,
    nod_materialization::ProtectedNodMaterializationV2,
    profile::poc_schema_limits,
};

fn carrier() -> ProtectedNodMaterializationV2 {
    ProtectedNodMaterializationV2 {
        queue_sequence: 7,
        first_nod_ordinal: 16,
        encryption_binding: B256::repeat_byte(0x31),
        encrypted_witness: BoundedBytes(vec![0xa1; 200]),
        encrypted_nods: vec![BoundedBytes(vec![0xb1; 96]), BoundedBytes(vec![0xb2; 96])],
    }
}

#[test]
fn protected_carrier_round_trips_without_becoming_a_plaintext_batch() {
    let limits = poc_schema_limits();
    let input = encode_protected_materialize_certified_nods_calldata(&carrier(), &limits).unwrap();
    assert_eq!(
        decode_protected_materialize_certified_nods_calldata(&input, &limits).unwrap(),
        carrier()
    );
    assert!(decode_materialize_certified_nods_calldata(&input, &limits).is_err());
}

#[test]
fn protected_carrier_rejects_ambiguous_or_truncated_encodings() {
    let limits = poc_schema_limits();
    let bytes = carrier().encode_canonical(&limits).unwrap();
    for length in 0..bytes.len() {
        assert!(
            ProtectedNodMaterializationV2::decode_canonical(&bytes[..length], &limits).is_err()
        );
    }
    for index in [0, 4, 6, 7] {
        let mut changed = bytes.clone();
        changed[index] ^= 1;
        assert!(ProtectedNodMaterializationV2::decode_canonical(&changed, &limits).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(ProtectedNodMaterializationV2::decode_canonical(&trailing, &limits).is_err());
}

#[test]
fn protected_carrier_requires_nonempty_bounded_ciphertexts_and_identity() {
    let limits = poc_schema_limits();
    let mut bad = carrier();
    bad.queue_sequence = 0;
    assert!(bad.encode_canonical(&limits).is_err());
    bad = carrier();
    bad.encryption_binding = B256::ZERO;
    assert!(bad.encode_canonical(&limits).is_err());
    bad = carrier();
    bad.encrypted_witness.0.clear();
    assert!(bad.encode_canonical(&limits).is_err());
    bad = carrier();
    bad.encrypted_nods.clear();
    assert!(bad.encode_canonical(&limits).is_err());
    bad = carrier();
    bad.encrypted_nods[0].0.clear();
    assert!(bad.encode_canonical(&limits).is_err());
    bad = carrier();
    bad.encrypted_nods.resize(257, BoundedBytes(vec![1]));
    assert!(bad.encode_canonical(&limits).is_err());
    let mut small = limits;
    small.codec.max_body_bytes = 64;
    assert!(carrier().encode_canonical(&small).is_err());
    assert!(ProtectedNodMaterializationV2::decode_canonical(
        &carrier().encode_canonical(&limits).unwrap(),
        &small,
    )
    .is_err());
}
