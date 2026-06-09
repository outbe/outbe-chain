#[test]
fn compile_fail_storage_handle_scope() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/storage_handle_thread_spawn.rs");
    t.compile_fail("tests/compile_fail/storage_handle_lifetime_escape.rs");
    t.compile_fail("tests/compile_fail/storage_facade_static_escape.rs");
}
