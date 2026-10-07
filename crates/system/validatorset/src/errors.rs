//! Module-local error types for ValidatorSet runtime/lifecycle.
//!
//! These typed errors serve protocol-level boundaries where
//! [`outbe_primitives::error::PrecompileError`] alone cannot express the failure
//! cleanly. For example: deterministic activation rejections in the consensus
//! stack, where the caller is `eyre`-based.
//!
//! `ActivationError` deliberately stays small and `#[non_exhaustive]`. A new
//! activation failure mode then does not break existing matches.

use outbe_primitives::error::PrecompileError;

/// Deterministic activation-time failures for the validator set.
///
/// `next_vrf_material_version` returns this error on overflow.
/// `activate_reshared_set` is a test helper and does not return it.
/// The production boundary hook does not return it either.
/// The consensus stack can reject the activation without panicking the node.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ActivationError {
    /// `vrf_material_version` reached `u64::MAX` and cannot be incremented.
    ///
    /// The activation must reject deterministically instead of saturating.
    /// Both proposer and validator paths then see the same failure and do not
    /// diverge on a silently capped value.
    #[error("vrf material version overflow at reshare activation")]
    VrfVersionOverflow,
}

impl From<ActivationError> for PrecompileError {
    fn from(err: ActivationError) -> Self {
        // ActivationError is unrecoverable at the EVM layer: the runtime cannot
        // re-derive valid VRF material in-place. Surface it as fatal.
        PrecompileError::Revert(err.to_string())
    }
}
