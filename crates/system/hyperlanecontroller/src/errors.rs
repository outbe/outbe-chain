//! Typed error definitions for the Hyperlane controller module.

use crate::abi::ICrosschainGov;
use alloy_primitives::Bytes;
use alloy_primitives::{Address, B256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the HyperlaneController runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HyperlaneControllerError {
    #[error("hyperlane controller is already initialized")]
    AlreadyInitialized,

    #[error("hyperlane controller is not initialized")]
    NotInitialized,

    #[error("local domain {domain} must be included in the ISM table")]
    LocalDomainMissing { domain: u32 },

    #[error("caller {caller} is not the bootstrap admin {admin}")]
    NotBootstrapAdmin { caller: Address, admin: Address },

    #[error("caller {caller} is not the current owner {owner} of the local ISM")]
    NotIsmOwner { caller: Address, owner: Address },

    #[error("local ISM pending owner is {pending}, expected the controller")]
    IsmNotPendingToController { pending: Address },

    #[error("contract {contract} is owned by {owner}, expected the controller")]
    NotOwnedByController { contract: Address, owner: Address },

    #[error("address must be non-zero")]
    InvalidAddress,

    #[error("domain must be non-zero")]
    InvalidDomain,

    #[error("domains and ISMs length mismatch")]
    IsmLengthMismatch,

    #[error("domains and hooks length mismatch")]
    HookLengthMismatch,

    #[error("no MerkleTreeHook configured for domain {domain}")]
    UnknownHook { domain: u32 },

    #[error("caller {caller} is not an active validator or its oracle delegate")]
    NotActiveValidator { caller: Address },

    #[error(
        "checkpoint signer {recovered} does not match validator {validator} signer {expected}"
    )]
    SignerMismatch {
        validator: Address,
        expected: Address,
        recovered: Address,
    },

    #[error("signer {signer} is already registered by validator {validator}")]
    SignerTaken { signer: Address, validator: Address },

    #[error("checkpoint index {index} is not newer than the submitted {submitted}")]
    StaleIndex { index: u32, submitted: u32 },

    #[error("checkpoint signature must be 65 bytes, got {length}")]
    InvalidSignatureLength { length: usize },

    #[error("domain {domain} is not configured")]
    UnknownDomain { domain: u32 },

    #[error("local domain {domain} cannot be removed or targeted remotely")]
    LocalDomainNotAllowed { domain: u32 },

    #[error("validator list must be non-empty and at most 255 entries, got {count}")]
    InvalidValidatorCount { count: usize },

    #[error("threshold {threshold} must be within 1..={validators}")]
    InvalidThreshold { threshold: u8, validators: usize },

    #[error("validator {validator} is zero or duplicated")]
    InvalidValidator { validator: Address },

    #[error("undecodable return data from {0}")]
    UndecodableReturn(&'static str),

    #[error("domain {domain} already has the ICA router {enrolled}")]
    RemoteRouterMismatch { domain: u32, enrolled: B256 },
}

impl From<HyperlaneControllerError> for PrecompileError {
    fn from(error: HyperlaneControllerError) -> Self {
        match error {
            HyperlaneControllerError::AlreadyInitialized => PrecompileError::RevertBytes(
                Bytes::from(ICrosschainGov::AlreadyInitialized {}.abi_encode()),
            ),
            HyperlaneControllerError::NotInitialized => PrecompileError::RevertBytes(Bytes::from(
                ICrosschainGov::NotInitialized {}.abi_encode(),
            )),
            HyperlaneControllerError::LocalDomainMissing { domain } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::LocalDomainMissing { domain }.abi_encode(),
                ))
            }
            HyperlaneControllerError::NotBootstrapAdmin { caller, admin } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::NotBootstrapAdmin { caller, admin }.abi_encode(),
                ))
            }
            HyperlaneControllerError::NotIsmOwner { caller, owner } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::NotIsmOwner { caller, owner }.abi_encode(),
                ))
            }
            HyperlaneControllerError::IsmNotPendingToController { pending } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::IsmNotPendingToController { pending }.abi_encode(),
                ))
            }
            HyperlaneControllerError::NotOwnedByController { contract, owner } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::NotOwnedByController {
                        target: contract,
                        owner,
                    }
                    .abi_encode(),
                ))
            }
            HyperlaneControllerError::InvalidAddress => PrecompileError::RevertBytes(Bytes::from(
                ICrosschainGov::InvalidAddress {}.abi_encode(),
            )),
            HyperlaneControllerError::InvalidDomain => PrecompileError::RevertBytes(Bytes::from(
                ICrosschainGov::InvalidDomain {}.abi_encode(),
            )),
            HyperlaneControllerError::IsmLengthMismatch => PrecompileError::RevertBytes(
                Bytes::from(ICrosschainGov::IsmLengthMismatch {}.abi_encode()),
            ),
            HyperlaneControllerError::HookLengthMismatch => PrecompileError::RevertBytes(
                Bytes::from(ICrosschainGov::HookLengthMismatch {}.abi_encode()),
            ),
            HyperlaneControllerError::UnknownHook { domain } => PrecompileError::RevertBytes(
                Bytes::from(ICrosschainGov::UnknownHook { domain }.abi_encode()),
            ),
            HyperlaneControllerError::NotActiveValidator { caller } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::NotActiveValidator { caller }.abi_encode(),
                ))
            }
            HyperlaneControllerError::SignerMismatch {
                validator,
                expected,
                recovered,
            } => PrecompileError::RevertBytes(Bytes::from(
                ICrosschainGov::SignerMismatch {
                    validator,
                    expected,
                    recovered,
                }
                .abi_encode(),
            )),
            HyperlaneControllerError::SignerTaken { signer, validator } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::SignerTaken { signer, validator }.abi_encode(),
                ))
            }
            HyperlaneControllerError::StaleIndex { index, submitted } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::StaleIndex { index, submitted }.abi_encode(),
                ))
            }
            HyperlaneControllerError::InvalidSignatureLength { length } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::InvalidSignatureLength {
                        length: length as u64,
                    }
                    .abi_encode(),
                ))
            }
            HyperlaneControllerError::UnknownDomain { domain } => PrecompileError::RevertBytes(
                Bytes::from(ICrosschainGov::UnknownDomain { domain }.abi_encode()),
            ),
            HyperlaneControllerError::LocalDomainNotAllowed { domain } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::LocalDomainNotAllowed { domain }.abi_encode(),
                ))
            }
            HyperlaneControllerError::InvalidValidatorCount { count } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::InvalidValidatorCount {
                        count: count as u64,
                    }
                    .abi_encode(),
                ))
            }
            HyperlaneControllerError::InvalidThreshold {
                threshold,
                validators,
            } => PrecompileError::RevertBytes(Bytes::from(
                ICrosschainGov::InvalidThreshold {
                    threshold,
                    validators: validators as u64,
                }
                .abi_encode(),
            )),
            HyperlaneControllerError::InvalidValidator { validator } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::InvalidValidator { validator }.abi_encode(),
                ))
            }
            HyperlaneControllerError::UndecodableReturn(value) => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::UndecodableReturn {
                        value: value.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            HyperlaneControllerError::RemoteRouterMismatch { domain, enrolled } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICrosschainGov::RemoteRouterMismatch { domain, enrolled }.abi_encode(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(err: HyperlaneControllerError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn caller_revert_keeps_address_integer_and_string_fields() {
        let caller = Address::repeat_byte(0x11);
        let admin = Address::repeat_byte(0x22);
        let bytes = encoded(HyperlaneControllerError::NotBootstrapAdmin { caller, admin });
        let decoded = ICrosschainGov::NotBootstrapAdmin::abi_decode(&bytes).expect("admin payload");
        assert_eq!(decoded.caller, caller);
        assert_eq!(decoded.admin, admin);

        let contract = Address::repeat_byte(0x33);
        let owner = Address::repeat_byte(0x44);
        let bytes = encoded(HyperlaneControllerError::NotOwnedByController { contract, owner });
        let decoded =
            ICrosschainGov::NotOwnedByController::abi_decode(&bytes).expect("owner payload");
        assert_eq!(decoded.target, contract);
        assert_eq!(decoded.owner, owner);

        let bytes = encoded(HyperlaneControllerError::InvalidSignatureLength { length: 65 });
        let decoded =
            ICrosschainGov::InvalidSignatureLength::abi_decode(&bytes).expect("length payload");
        assert_eq!(decoded.length, 65);

        let bytes = encoded(HyperlaneControllerError::UndecodableReturn("short return"));
        let decoded =
            ICrosschainGov::UndecodableReturn::abi_decode(&bytes).expect("string payload");
        assert_eq!(decoded.value, "short return");
    }
}
