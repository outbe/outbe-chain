//! Typed error definitions for the FsEpochRegistry precompile.

use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::precompile::IFsEpochRegistry;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FsEpochRegistryError {
    #[error("caller is not admin")]
    NotAdmin,

    #[error("address must be non-zero")]
    ZeroAddress,

    #[error("epoch 0 is already anchored")]
    AlreadyAnchored,

    #[error("epoch 0 has not been anchored yet")]
    NotAnchored,

    #[error("pkAtt must be exactly 256 bytes (EIP-2537 G2)")]
    InvalidKeyLength,

    #[error("pkAtt is not a valid non-identity G2 subgroup point")]
    InvalidKey,

    #[error("dst must be between 1 and 255 bytes")]
    InvalidDst,

    #[error("epoch {epoch} does not match stored digest")]
    EpochMismatch { epoch: u32 },

    #[error("epoch {epoch} prevHash does not match previous epoch digest")]
    BadPrevHash { epoch: u32 },

    #[error("epoch {epoch} startWwd does not strictly increase")]
    StartWwdNotIncreasing { epoch: u32 },

    #[error("epoch {epoch} sigPrev failed BLS verification")]
    BadEpochSignature { epoch: u32 },

    #[error("epoch {epoch} skips past head {head}")]
    EpochGap { epoch: u32, head: u32 },

    #[error("epoch {epoch} is unknown")]
    EpochUnknown { epoch: u32 },
}

impl From<FsEpochRegistryError> for PrecompileError {
    fn from(error: FsEpochRegistryError) -> Self {
        match error {
            FsEpochRegistryError::NotAdmin => PrecompileError::RevertBytes(Bytes::from(
                IFsEpochRegistry::NotAdmin {}.abi_encode(),
            )),
            FsEpochRegistryError::ZeroAddress => PrecompileError::RevertBytes(Bytes::from(
                IFsEpochRegistry::ZeroAddress {}.abi_encode(),
            )),
            FsEpochRegistryError::AlreadyAnchored => PrecompileError::RevertBytes(Bytes::from(
                IFsEpochRegistry::AlreadyAnchored {}.abi_encode(),
            )),
            FsEpochRegistryError::NotAnchored => PrecompileError::RevertBytes(Bytes::from(
                IFsEpochRegistry::NotAnchored {}.abi_encode(),
            )),
            FsEpochRegistryError::InvalidKeyLength => PrecompileError::RevertBytes(Bytes::from(
                IFsEpochRegistry::InvalidKeyLength {}.abi_encode(),
            )),
            FsEpochRegistryError::InvalidKey => PrecompileError::RevertBytes(Bytes::from(
                IFsEpochRegistry::InvalidKey {}.abi_encode(),
            )),
            FsEpochRegistryError::InvalidDst => PrecompileError::RevertBytes(Bytes::from(
                IFsEpochRegistry::InvalidDst {}.abi_encode(),
            )),
            FsEpochRegistryError::EpochMismatch { epoch } => PrecompileError::RevertBytes(
                Bytes::from(IFsEpochRegistry::EpochMismatch { epoch }.abi_encode()),
            ),
            FsEpochRegistryError::BadPrevHash { epoch } => PrecompileError::RevertBytes(
                Bytes::from(IFsEpochRegistry::BadPrevHash { epoch }.abi_encode()),
            ),
            FsEpochRegistryError::StartWwdNotIncreasing { epoch } => PrecompileError::RevertBytes(
                Bytes::from(IFsEpochRegistry::StartWwdNotIncreasing { epoch }.abi_encode()),
            ),
            FsEpochRegistryError::BadEpochSignature { epoch } => PrecompileError::RevertBytes(
                Bytes::from(IFsEpochRegistry::BadEpochSignature { epoch }.abi_encode()),
            ),
            FsEpochRegistryError::EpochGap { epoch, head } => PrecompileError::RevertBytes(
                Bytes::from(IFsEpochRegistry::EpochGap { epoch, head }.abi_encode()),
            ),
            FsEpochRegistryError::EpochUnknown { epoch } => PrecompileError::RevertBytes(
                Bytes::from(IFsEpochRegistry::EpochUnknown { epoch }.abi_encode()),
            ),
        }
    }
}
