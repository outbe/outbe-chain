//! Hardware-free process harness, absent from every release profile.
fn main() {
    if let Err(error) = outbe_tee_enclave::local_e2e::run() {
        eprintln!("LOCAL_E2E_FAILED: {error}");
        std::process::exit(1);
    }
}
