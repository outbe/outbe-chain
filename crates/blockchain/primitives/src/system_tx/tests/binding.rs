use crate as primitives;
use crate::system_tx::{binding::*, OcompLifecycleActivation, SystemTxLayout};
use alloy_primitives::B256;
#[path = "binding_cases.rs"]
mod cases;

fn fixture(name: &str) -> cases::Case {
    cases::cases()
        .into_iter()
        .find(|case| case.name == name)
        .expect("binding fixture exists")
}

#[test]
fn shared_diagnostics_match_original_node_snapshots() {
    cases::assert_diagnostics(
        true,
        |body, header, activation| {
            validate_system_layout(body, header, activation)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
        |layout, header| {
            validate_parent_accounting_binding(layout, header).map_err(|error| match error {
                ParentAccountingBindingError::Missing { block_number } => {
                    format!("missing CertifiedParentAccounting system tx for block {block_number}")
                }
                error => error.to_string(),
            })
        },
        |layout, artifacts| {
            validate_boundary_outcome_binding(layout, artifacts).map_err(|error| error.to_string())
        },
    );
}

#[test]
fn layout_failures_identify_the_rejected_stage() {
    let artifacts = fixture("artifacts_before_layout");
    assert!(matches!(
        validate_system_layout(
            &artifacts.body,
            &artifacts.header,
            OcompLifecycleActivation::Disabled
        ),
        Err(LayoutBindingError::Artifacts(_))
    ));
    let order = fixture("wrong_order");
    assert!(matches!(
        validate_system_layout(
            &order.body,
            &order.header,
            OcompLifecycleActivation::Disabled
        ),
        Err(LayoutBindingError::Layout(_))
    ));
    let set = fixture("block1_requires_boundary");
    assert!(matches!(
        validate_system_layout(&set.body, &set.header, OcompLifecycleActivation::Disabled),
        Err(LayoutBindingError::Set(_))
    ));
}

#[test]
fn parent_errors_carry_block_height_and_both_hashes() {
    let missing = fixture("parent_missing");
    assert!(matches!(
        validate_parent_accounting_binding(&missing.layout(), &missing.header),
        Err(ParentAccountingBindingError::Missing { block_number: 2 })
    ));
    let mismatch = fixture("parent_hash_mismatch");
    let Err(ParentAccountingBindingError::HashMismatch { expected, actual }) =
        validate_parent_accounting_binding(&mismatch.layout(), &mismatch.header)
    else {
        panic!("expected parent hash mismatch");
    };
    assert_eq!(expected, B256::repeat_byte(0x41));
    assert_eq!(actual, B256::repeat_byte(0x42));
}

#[test]
fn boundary_parity_checks_end_inputs_after_a_matching_begin_input() {
    let case = fixture("boundary_match_then_bad_input");
    let layout = SystemTxLayout {
        begin: vec![&case.body.transactions[0]],
        user: vec![],
        end: vec![&case.body.transactions[1]],
    };
    assert!(matches!(
        validate_boundary_outcome_binding(&layout, &case.artifacts),
        Err(BoundaryBindingError::Decode(_))
    ));
}

#[test]
fn boundary_rejects_the_first_failure_in_system_transaction_order() {
    let mismatch = fixture("boundary_mismatch");
    let malformed = fixture("no_boundary_skips_malformed_inputs");
    let bad_artifact = &mismatch.body.transactions[0];
    let bad_input = &malformed.body.transactions[0];
    let mut layout = SystemTxLayout {
        begin: vec![bad_input, bad_artifact],
        user: vec![],
        end: vec![],
    };
    assert!(matches!(
        validate_boundary_outcome_binding(&layout, &mismatch.artifacts),
        Err(BoundaryBindingError::Decode(_))
    ));
    layout.begin.reverse();
    assert!(matches!(
        validate_boundary_outcome_binding(&layout, &mismatch.artifacts),
        Err(BoundaryBindingError::Mismatch)
    ));
}
