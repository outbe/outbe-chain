//! Typed error definitions for the CodeRegistry system precompile.

use crate::precompile::ICodeRegistry;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the CodeRegistry runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CodeRegistryError {
    #[error("Radicle repository id must be non-zero")]
    ZeroRepositoryId,
    #[error("Radicle repository is already registered")]
    AlreadyRegistered,
    #[error("Radicle repository registry capacity {maximum} reached")]
    CapacityReached { maximum: u32 },
    #[error("Radicle repository index {index} is outside count {count}")]
    IndexOutOfBounds { index: u32, count: u32 },
    #[error("Radicle repository storage key has a non-zero tail")]
    NonZeroStorageKeyTail,
    #[error("Radicle repository index {index} is inconsistent")]
    InconsistentRepositoryIndex { index: u32 },
    #[error("invalid or noncanonical Radicle patch Vote payload")]
    InvalidPatchPayload,
    #[error("Radicle patch Vote repository is not registered")]
    UnknownRepository,
    #[error("Radicle patch approval does not match the stored Vote proposal")]
    ApprovalProposalMismatch,
    #[error("Radicle patch approval is immutable")]
    ImmutableApproval,
    #[error("invalid Radicle publication: {0}")]
    InvalidPublication(&'static str),
    #[error("noncanonical governments storage bytes")]
    NoncanonicalGovernmentsBytes,
    #[error("governments descriptor commitment mismatch")]
    GovernmentsDescriptorMismatch,
}

impl From<CodeRegistryError> for PrecompileError {
    fn from(error: CodeRegistryError) -> Self {
        match error {
            CodeRegistryError::ZeroRepositoryId => PrecompileError::RevertBytes(Bytes::from(
                ICodeRegistry::ZeroRepositoryId {}.abi_encode(),
            )),
            CodeRegistryError::AlreadyRegistered => PrecompileError::RevertBytes(Bytes::from(
                ICodeRegistry::AlreadyRegistered {}.abi_encode(),
            )),
            CodeRegistryError::CapacityReached { maximum } => PrecompileError::RevertBytes(
                Bytes::from(ICodeRegistry::CapacityReached { maximum }.abi_encode()),
            ),
            CodeRegistryError::IndexOutOfBounds { index, count } => PrecompileError::RevertBytes(
                Bytes::from(ICodeRegistry::IndexOutOfBounds { index, count }.abi_encode()),
            ),
            CodeRegistryError::NonZeroStorageKeyTail => PrecompileError::RevertBytes(Bytes::from(
                ICodeRegistry::NonZeroStorageKeyTail {}.abi_encode(),
            )),
            CodeRegistryError::InconsistentRepositoryIndex { index } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICodeRegistry::InconsistentRepositoryIndex { index }.abi_encode(),
                ))
            }
            CodeRegistryError::InvalidPatchPayload => PrecompileError::RevertBytes(Bytes::from(
                ICodeRegistry::InvalidPatchPayload {}.abi_encode(),
            )),
            CodeRegistryError::UnknownRepository => PrecompileError::RevertBytes(Bytes::from(
                ICodeRegistry::UnknownRepository {}.abi_encode(),
            )),
            CodeRegistryError::ApprovalProposalMismatch => PrecompileError::RevertBytes(
                Bytes::from(ICodeRegistry::ApprovalProposalMismatch {}.abi_encode()),
            ),
            CodeRegistryError::ImmutableApproval => PrecompileError::RevertBytes(Bytes::from(
                ICodeRegistry::ImmutableApproval {}.abi_encode(),
            )),
            CodeRegistryError::InvalidPublication(value) => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICodeRegistry::InvalidPublication {
                        value: value.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            CodeRegistryError::NoncanonicalGovernmentsBytes => PrecompileError::RevertBytes(
                Bytes::from(ICodeRegistry::NoncanonicalGovernmentsBytes {}.abi_encode()),
            ),
            CodeRegistryError::GovernmentsDescriptorMismatch => PrecompileError::RevertBytes(
                Bytes::from(ICodeRegistry::GovernmentsDescriptorMismatch {}.abi_encode()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(err: CodeRegistryError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn caller_revert_keeps_integer_and_string_fields() {
        let bytes = encoded(CodeRegistryError::IndexOutOfBounds { index: 4, count: 2 });
        let decoded = ICodeRegistry::IndexOutOfBounds::abi_decode(&bytes).expect("index payload");
        assert_eq!(decoded.index, 4);
        assert_eq!(decoded.count, 2);

        let bytes = encoded(CodeRegistryError::InvalidPublication(
            "material already bound",
        ));
        let decoded =
            ICodeRegistry::InvalidPublication::abi_decode(&bytes).expect("string payload");
        assert_eq!(decoded.value, "material already bound");

        let bytes = encoded(CodeRegistryError::NoncanonicalGovernmentsBytes);
        assert_eq!(
            &bytes[..4],
            &ICodeRegistry::NoncanonicalGovernmentsBytes::SELECTOR
        );
    }
}
