#[test]
fn proposal_payload_trace_exposes_the_exact_fcu_payload_id() {
    let trace = super::ProposalPayloadTrace::default();
    let payload_id = alloy_rpc_types_engine::PayloadId::new([0x85; 8]);

    assert_eq!(trace.payload_id(), None);
    trace.record(payload_id);
    assert_eq!(trace.payload_id(), Some(payload_id));
}
