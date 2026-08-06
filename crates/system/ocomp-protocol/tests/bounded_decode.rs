// OCOMP-TEST-ID: OCM-BND-003

use outbe_ocomp_protocol::{
    codec::{CanonicalReader, CodecLimits},
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
fn vote_state_rejects_a_fifth_slot_before_any_crypto_work() {
    let mut accountability =
        OcompVoteAccountabilityV1::empty([1; 32].into(), [2; 32].into()).unwrap();
    accountability.slots.push(None);
    let error = accountability
        .validate_semantics(&LIMITS)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "invalid invariant: accountability slot count");
}
