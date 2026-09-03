#[test]
fn compile_fail_storage_handle_scope() {
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        eprintln!("compile contracts run separately from runtime coverage");
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/storage_handle_thread_spawn.rs");
    t.compile_fail("tests/compile_fail/storage_handle_lifetime_escape.rs");
    t.compile_fail("tests/compile_fail/storage_facade_static_escape.rs");
    t.compile_fail("tests/compile_fail/lysis_activation_private_constructor.rs");
    t.compile_fail("tests/compile_fail/lysis_activation_not_clone.rs");
    t.compile_fail("tests/compile_fail/lysis_activation_frame_escape.rs");
}
