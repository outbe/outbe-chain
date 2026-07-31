#[test]
fn ocomp_world_exposes_no_direct_job_result_or_state_injection() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/ocomp_direct_injection_*.rs");
}
