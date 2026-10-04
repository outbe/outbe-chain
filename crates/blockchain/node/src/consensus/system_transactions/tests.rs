use outbe_primitives as primitives;
#[path = "../../../../primitives/src/system_tx/tests/binding_cases.rs"]
mod cases;

#[test]
fn binding_diagnostics_match_pre_refactor_snapshots() {
    cases::assert_diagnostics(
        true,
        |body, header, activation| {
            super::validate_layout(body, header, activation)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
        |layout, header| {
            super::validate_parent_accounting(layout, header).map_err(|error| error.to_string())
        },
        |layout, artifacts| {
            super::validate_boundary_outcome(layout, artifacts).map_err(|error| error.to_string())
        },
    );
}
