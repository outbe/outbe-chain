use crate::internal::eth::ViewCallOutcome;
use crate::world::rpc::*;
use alloy_sol_types::SolError as _;

#[test]
fn first_owner_index_revert_is_not_reported_as_an_absent_nod() {
    let error =
        classify_owner_index_result(0, Err("execution reverted: index out of bounds".to_owned()))
            .expect_err("index zero must preserve the execution failure");

    assert!(error.contains("index out of bounds"));
    let error = classify_owner_index_result(
        0,
        Ok(ViewCallOutcome::Reverted(
            INod::IndexOutOfBounds {}.abi_encode().into(),
        )),
    )
    .expect_err("the first Nod must exist even for a typed bounds revert");
    assert!(error.contains("0x4e23d035"));
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
        classify_owner_index_result(
            1,
            Ok(ViewCallOutcome::Reverted(
                INod::IndexOutOfBounds {}.abi_encode().into()
            )),
        )
        .expect("the canonical bounds error is an expected absence"),
        None,
    );
}

#[test]
fn second_owner_index_rejects_unknown_and_malformed_reverts() {
    let mut trailing = INod::IndexOutOfBounds {}.abi_encode();
    trailing.push(0);
    for data in [vec![0xde, 0xad, 0xbe, 0xef], trailing, Vec::new()] {
        classify_owner_index_result(1, Ok(ViewCallOutcome::Reverted(data.into())))
            .expect_err("only the exact bounds revert proves absence");
    }
    classify_owner_index_result(1, Err("execution reverted: index out of bounds".to_owned()))
        .expect_err("unstructured text is not an authenticated EVM bounds revert");
}
