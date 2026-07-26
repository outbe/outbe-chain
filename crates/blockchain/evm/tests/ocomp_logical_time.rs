use outbe_metadosis::ocomp::test_support::{
    ActivationFixture, ActivationMetadata, TEST_LOGICAL_TIME,
};
use outbe_ocomp_protocol::receipts::ActivationOutcome;

// OCOMP-TEST-ID: OCM-TIM-001
#[test]
fn request_pinned_semantics_are_identical_at_different_activation_heights() {
    let mut first = ActivationFixture::new(20, 1_010, true);
    let first_calldata = first.calldata();
    let first_output = first.apply().expect("first activation must apply");
    assert_eq!(
        ActivationFixture::decoded_outcome(&first_output),
        ActivationOutcome::Applied
    );
    let first_semantics = first.semantic_snapshot();
    let first_metadata = first.activation_metadata();
    let first_state = first.rollback_snapshot();

    let mut second = ActivationFixture::new(40, 2_020, true);
    assert_eq!(
        second.calldata(),
        first_calldata,
        "activation height is not part of the request-pinned computation"
    );
    let second_output = second.apply().expect("second activation must apply");
    assert_eq!(
        ActivationFixture::decoded_outcome(&second_output),
        ActivationOutcome::Applied
    );
    let second_semantics = second.semantic_snapshot();
    let second_metadata = second.activation_metadata();
    let second_state = second.rollback_snapshot();

    assert_eq!(
        first_semantics, second_semantics,
        "all owner effects must use request-pinned logical time"
    );
    assert_eq!(first_semantics.nod.issued_at, TEST_LOGICAL_TIME);
    assert_eq!(
        first_metadata,
        ActivationMetadata {
            activated_at_height: 20,
            activated_at_time: 1_010,
        }
    );
    assert_eq!(
        second_metadata,
        ActivationMetadata {
            activated_at_height: 40,
            activated_at_time: 2_020,
        }
    );
    assert_ne!(
        first_state, second_state,
        "terminal activation metadata must remain observable"
    );
}
