// OCOMP-TEST-ID: OCM-BND-003

use outbe_ocomp_protocol::{
    codec::{CanonicalReader, CodecLimits},
    control::{ControlFrameV1, ControlMagic},
    vote::OcompVoteAccountabilityV1,
    SchemaLimits,
};

const CODEC: CodecLimits = CodecLimits::new(4_096, 4, 4_096);
const LIMITS: SchemaLimits = SchemaLimits {
    codec: CODEC,
    max_bounded_bytes: 8,
    max_proof_bytes: 8,
    max_opening_bytes: 8,
    max_collection_items: 4,
    max_action_items: 4,
    max_chunk_items: 4,
    max_unit_inputs: 4,
    max_result_chunk_bytes: 8,
    max_control_body_bytes: 8,
};

#[test]
fn collection_cap_plus_one_rejects_before_reserving_items() {
    let encoded = 5_u32.to_be_bytes();
    let mut reader = CanonicalReader::new(&encoded, CODEC).unwrap();
    assert!(reader
        .read_vec::<u64>(4, 8, |input| input.read_u64())
        .is_err());
    assert_eq!(reader.allocation_stats().reservations, 0);
    assert_eq!(reader.allocation_stats().reserved_bytes, 0);
}

#[test]
fn control_cap_plus_one_rejects_from_prefix_before_body_decode() {
    let frame = ControlFrameV1 {
        magic: ControlMagic::Worker,
        message_kind: 0x0010,
        session_generation: 1,
        request_id: 1,
        body: vec![0; LIMITS.max_control_body_bytes],
    };
    let mut encoded = frame.encode(&LIMITS).unwrap();
    let claimed = u32::try_from(28 + LIMITS.max_control_body_bytes + 1).unwrap();
    encoded[..4].copy_from_slice(&claimed.to_be_bytes());
    assert!(ControlFrameV1::decode(&encoded, ControlMagic::Worker, &LIMITS).is_err());
}

#[test]
fn vote_state_rejects_slot_count_different_from_declared_n_before_crypto_work() {
    let mut accountability =
        OcompVoteAccountabilityV1::empty([1; 32].into(), 1, [2; 32].into(), [3; 32].into(), 4, 3)
            .unwrap();
    accountability.slots.push(None);
    let error = accountability
        .validate_semantics(&LIMITS)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "invalid invariant: accountability slot count");
}

#[test]
fn checked_arithmetic_rejects_overflowing_frame_cap() {
    let oversized = SchemaLimits {
        max_control_body_bytes: usize::MAX,
        ..LIMITS
    };
    let frame = ControlFrameV1 {
        magic: ControlMagic::Worker,
        message_kind: 0x0010,
        session_generation: 1,
        request_id: 1,
        body: Vec::new(),
    };
    assert!(frame.encode(&oversized).is_ok());
    assert!(ControlFrameV1::decode(&[0; 4], ControlMagic::Worker, &oversized).is_err());
}
