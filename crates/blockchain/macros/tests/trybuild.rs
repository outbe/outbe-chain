//! Compile-fail tests for `#[contract_dispatch]` and its helper attributes.
//!
//! Each case triggers an error raised by the macro before any code is emitted,
//! so external crates (`outbe-primitives`, `alloy-sol-types`) are not required.

#[test]
fn compile_fail_contract_dispatch() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/view_and_payable_conflict.rs");
    t.compile_fail("tests/compile_fail/mutating_missing_caller.rs");
    t.compile_fail("tests/compile_fail/arg_count_mismatch.rs");
}
