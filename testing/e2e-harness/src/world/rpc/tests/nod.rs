use crate::world::rpc::*;

#[test]
fn first_owner_index_revert_is_not_reported_as_an_absent_nod() {
    let error =
        classify_owner_index_result(0, Err("execution reverted: index out of bounds".to_owned()))
            .expect_err("index zero must preserve the execution failure");

    assert!(error.contains("index out of bounds"));
}

#[test]
fn second_owner_index_transport_failure_does_not_prove_uniqueness() {
    let error = classify_owner_index_result(
        1,
        Err("native state unavailable: exact parent mismatch".to_owned()),
    )
    .expect_err("a readiness failure must not be treated as an absent second NOD");

    assert!(error.contains("exact parent mismatch"));
}

#[test]
fn exact_second_owner_index_out_of_bounds_proves_uniqueness() {
    assert_eq!(
        classify_owner_index_result(1, Err("execution reverted: index out of bounds".to_owned()),)
            .expect("the canonical bounds error is an expected absence"),
        None,
    );
}
