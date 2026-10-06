//! V2 Phase 1 accounting errors.
//!
//! The scope needs no module-local error type. Every mutating call returns
//! [`outbe_primitives::error::PrecompileError`] from the
//! underlying [`outbe_primitives::storage::types::Slot`] read/write.
//! The module is reserved so that future cross-module call surfaces can add
//! typed errors without re-organizing the crate.
