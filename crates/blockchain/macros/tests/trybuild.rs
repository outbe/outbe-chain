//! Compile-fail tests for `#[contract_dispatch]`, its helper attributes and
//! the storage layout assertion of `#[contract]`.

#[test]
fn compile_fail_contract_dispatch() {
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        eprintln!("compile contracts run separately from runtime coverage");
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/view_and_payable_conflict.rs");
    t.compile_fail("tests/compile_fail/mutating_missing_caller.rs");
    t.compile_fail("tests/compile_fail/arg_count_mismatch.rs");
    t.compile_fail("tests/compile_fail/payable_without_selectors.rs");
    t.compile_fail("tests/compile_fail/explicit_slot_inside_array.rs");
}
