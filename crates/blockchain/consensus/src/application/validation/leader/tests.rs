use outbe_primitives as primitives;
#[path = "../../../../../primitives/src/system_tx/tests/binding_cases.rs"]
mod cases;

#[test]
fn binding_diagnostics_match_pre_refactor_snapshots() {
    cases::assert_diagnostics(
        false,
        |body, header, activation| super::validate_layout(body, header, activation).map(|_| ()),
        super::validate_parent_accounting,
        super::validate_boundary_outcome,
    );
}
