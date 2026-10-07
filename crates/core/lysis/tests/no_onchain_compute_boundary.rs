mod support;

use alloy_primitives::B256;
use outbe_lysis::activation_v1::{verify_result, LysisApplyPlanV1, LysisResultInputsV1};
use outbe_ocomp_protocol::{
    intent::{DayType, JobIntentV1},
    result::{ActivationPayloadV1, LysisResultV1},
    ProtocolError, SchemaLimits,
};

use support::{activation_fixture, hash, recommit_result, ActivationFixtureV1};

type StorageFreeVerifier = fn(LysisResultInputsV1<'_>) -> Result<LysisApplyPlanV1, ProtocolError>;

// Each input is bound to its type, and the literal names every field: a further
// verifier input, such as a storage handle, stops this from compiling.
fn closed_inputs<'a>(
    fixture: &'a ActivationFixtureV1,
    activation_payload: &'a ActivationPayloadV1,
    result: &'a LysisResultV1,
) -> LysisResultInputsV1<'a> {
    let intent_id: B256 = fixture.intent_id;
    let expected_job_id: B256 = fixture.job_id;
    let intent: &JobIntentV1 = &fixture.intent;
    let limits: &SchemaLimits = &fixture.limits;
    let nod_issued_at: u64 = fixture.nod_issued_at;
    LysisResultInputsV1 {
        intent_id,
        expected_job_id,
        intent,
        activation_payload,
        result,
        limits,
        nod_issued_at,
    }
}

// OCOMP-TEST-ID: OCM-BND-002
#[test]
fn activation_verifier_has_a_storage_free_closed_input_boundary() {
    let verifier: StorageFreeVerifier = verify_result;
    let fixture = activation_fixture(DayType::Green);

    let plan = verifier(closed_inputs(&fixture, &fixture.payload, &fixture.result)).unwrap();
    assert_eq!(
        plan.request_limit_split_receipt_hash(),
        fixture
            .request_receipt
            .receipt_hash(&fixture.limits)
            .unwrap()
    );
    assert_eq!(
        plan.binding().activation_call_id,
        plan.call_core()
            .activation_call_id(&fixture.limits)
            .unwrap()
    );

    let mut rebound_payload = fixture.payload.clone();
    rebound_payload.roots.output_manifest_root = hash(230);
    assert!(verifier(closed_inputs(&fixture, &rebound_payload, &fixture.result)).is_err());

    let mut rebound_result = fixture.result.clone();
    rebound_result.roots.output_manifest_root = hash(231);
    recommit_result(&mut rebound_result, &fixture.limits);
    let rebound_result_payload = rebound_result.activation_payload(&fixture.limits).unwrap();
    assert!(verifier(closed_inputs(
        &fixture,
        &rebound_result_payload,
        &fixture.result
    ))
    .is_err());
}
