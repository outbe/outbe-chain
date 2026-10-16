//! Separate test process: raw verification must never initialize or fetch CRS.

use outbe_zk_backend::barretenberg::{Barretenberg, BarretenbergHandler, RawVerifier};
use outbe_zk_backend::errors::HandlerError;

#[test]
fn raw_verification_requires_explicit_initialization() {
    assert!(matches!(
        BarretenbergHandler::initialized(),
        Err(HandlerError::NotInitialized)
    ));
    let error = Barretenberg::default()
        .verify_combined(b"vk", &0u32.to_be_bytes())
        .unwrap_err();
    assert!(error.to_string().contains("handler is not initialized"));
    assert!(matches!(
        BarretenbergHandler::initialized(),
        Err(HandlerError::NotInitialized)
    ));
}
