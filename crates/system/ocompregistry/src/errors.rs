//! Typed error constructors for the OcompRegistry system precompile.

use outbe_primitives::error::PrecompileError;

/// Creates a revert precompile error for corrupted OcompRegistry storage state.
pub(crate) fn corruption(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Revert(message.into())
}
