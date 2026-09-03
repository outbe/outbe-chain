fn compile_contracts_run_separately_from_coverage() -> bool {
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        eprintln!("compile contracts run separately from runtime coverage");
        true
    } else {
        false
    }
}

#[test]
fn external_mutation_surface_compile_fail_examples() {
    if compile_contracts_run_separately_from_coverage() {
        return;
    }
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/metadosis_raw_surface.rs");
    cases.compile_fail("tests/ui/metadosis_mutation_lease.rs");
    cases.compile_fail("tests/ui/metadosis_mutation_purpose.rs");
    cases.compile_fail("tests/ui/metadosis_commit_permit.rs");
}

#[cfg(feature = "test-utils")]
#[test]
fn test_utils_keeps_the_raw_fixture_kernel_private() {
    if compile_contracts_run_separately_from_coverage() {
        return;
    }
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/metadosis_fixture_kernel.rs");
    cases.compile_fail("tests/ui/metadosis_semantic_kernel.rs");
}
