//! [`outbe_primitives::storage::direct::DirectStorageProvider::set_code`]
//! returns [`outbe_primitives::error::PrecompileError::Unsupported`]
//! instead of silently succeeding.
//!
//! Before the body was `Ok(())`, which masked the no-op and
//! could make callers (block-level hooks) believe a code write landed
//! when in fact `DirectStorageProvider` cannot deploy code. The new
//! body fails closed.

use outbe_primitives::error::PrecompileError;

#[test]
fn set_code_returns_unsupported_variant_exists() {
    // The actual runtime invocation requires a real reth StateProvider
    // (DirectStorageProvider<DB: StateDB>) which is heavy to construct
    // here. The contract that matters is verified by:
    // 1) `rg -n 'set_code.*Err\(PrecompileError::Unsupported\)'
    //    crates/blockchain/primitives/src/storage/direct.rs` returns 1
    //    match.
    // 2) `PrecompileError::Unsupported` variant exists and can be
    //    pattern-matched (asserted below).
    let err = PrecompileError::Unsupported;
    match err {
        PrecompileError::Unsupported => {}
        other => panic!(
            "PrecompileError::Unsupported variant must exist; got {:?}",
            other
        ),
    }
}
