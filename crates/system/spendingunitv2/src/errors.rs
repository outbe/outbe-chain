//! Typed error definitions for the SpendingUnitV2 precompile.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::precompile::ISpendingUnitV2;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SpendingUnitV2Error {
    #[error("commitment token already exists")]
    AlreadyExists,
    #[error("invalid commitment token id")]
    InvalidTokenId,

    #[error("attester is not active")]
    AttesterNotActive,
    #[error("attester lacks SubmitSpendingUnit permission")]
    AttesterNotPermitted,

    #[error("contract is paused")]
    EnforcedPause,
    #[error("contract is not paused")]
    ExpectedPause,

    #[error("caller {account} is not owner")]
    OwnableUnauthorizedAccount { account: Address },
    #[error("invalid owner {owner}")]
    OwnableInvalidOwner { owner: Address },

    #[error("field element {value} is not canonical (< BN254 scalar modulus)")]
    NonCanonicalFieldElement { value: B256 },

    #[error("invalid amount")]
    InvalidAmount,
    #[error("invalid currency")]
    InvalidCurrency,
    #[error("derivedOwner must be non-zero")]
    InvalidDerivedOwner,
    #[error("context {ctx} already settled under {settled_id}")]
    AlreadySettled { ctx: B256, settled_id: B256 },
    #[error("seal totals do not match batch totals")]
    BatchTotalsMismatch,
    #[error("enclave {enclave_pk} is not active for attester {attester}")]
    EnclaveNotActiveFor { enclave_pk: B256, attester: Address },
    #[error("pool epoch {pool_epoch} is retired")]
    PoolEpochRetired { pool_epoch: u32 },
    #[error("pool epoch {pool_epoch} opened after worldwide day {worldwide_day}")]
    PoolEpochAfterDay { pool_epoch: u32, worldwide_day: u64 },
    #[error("committee handle signature invalid for pool epoch {pool_epoch}")]
    HandleSignatureInvalid { pool_epoch: u32 },
    #[error("enclave settlement signature invalid for {enclave_pk}")]
    SettlementSignatureInvalid { enclave_pk: B256 },
    #[error("malformed input")]
    MalformedInput,
    #[error("multicall entry {index} is not submitSU")]
    MulticallMethodNotAllowed { index: U256 },

    #[error("attester registry address is zero")]
    ZeroAttesterRegistry,
}

impl From<SpendingUnitV2Error> for PrecompileError {
    fn from(error: SpendingUnitV2Error) -> Self {
        use SpendingUnitV2Error::*;
        match error {
            ZeroAttesterRegistry => PrecompileError::Revert(error.to_string()),
            other => {
                let encoded = match other {
                    AlreadyExists => ISpendingUnitV2::AlreadyExists {}.abi_encode(),
                    InvalidTokenId => ISpendingUnitV2::InvalidTokenId {}.abi_encode(),
                    AttesterNotActive => ISpendingUnitV2::AttesterNotActive {}.abi_encode(),
                    AttesterNotPermitted => ISpendingUnitV2::AttesterNotPermitted {}.abi_encode(),
                    EnforcedPause => ISpendingUnitV2::EnforcedPause {}.abi_encode(),
                    ExpectedPause => ISpendingUnitV2::ExpectedPause {}.abi_encode(),
                    OwnableUnauthorizedAccount { account } => {
                        ISpendingUnitV2::OwnableUnauthorizedAccount { account }.abi_encode()
                    }
                    OwnableInvalidOwner { owner } => {
                        ISpendingUnitV2::OwnableInvalidOwner { owner }.abi_encode()
                    }
                    NonCanonicalFieldElement { value } => {
                        ISpendingUnitV2::NonCanonicalFieldElement { value }.abi_encode()
                    }
                    InvalidAmount => ISpendingUnitV2::InvalidAmount {}.abi_encode(),
                    InvalidCurrency => ISpendingUnitV2::InvalidCurrency {}.abi_encode(),
                    InvalidDerivedOwner => ISpendingUnitV2::InvalidDerivedOwner {}.abi_encode(),
                    AlreadySettled { ctx, settled_id } => ISpendingUnitV2::AlreadySettled {
                        ctx,
                        settledId: settled_id,
                    }
                    .abi_encode(),
                    BatchTotalsMismatch => ISpendingUnitV2::BatchTotalsMismatch {}.abi_encode(),
                    EnclaveNotActiveFor {
                        enclave_pk,
                        attester,
                    } => ISpendingUnitV2::EnclaveNotActiveFor {
                        enclavePk: enclave_pk,
                        attester,
                    }
                    .abi_encode(),
                    PoolEpochRetired { pool_epoch } => ISpendingUnitV2::PoolEpochRetired {
                        poolEpoch: pool_epoch,
                    }
                    .abi_encode(),
                    PoolEpochAfterDay {
                        pool_epoch,
                        worldwide_day,
                    } => ISpendingUnitV2::PoolEpochAfterDay {
                        poolEpoch: pool_epoch,
                        worldwideDay: worldwide_day,
                    }
                    .abi_encode(),
                    HandleSignatureInvalid { pool_epoch } => {
                        ISpendingUnitV2::HandleSignatureInvalid {
                            poolEpoch: pool_epoch,
                        }
                        .abi_encode()
                    }
                    SettlementSignatureInvalid { enclave_pk } => {
                        ISpendingUnitV2::SettlementSignatureInvalid {
                            enclavePk: enclave_pk,
                        }
                        .abi_encode()
                    }
                    MalformedInput => ISpendingUnitV2::MalformedInput {}.abi_encode(),
                    MulticallMethodNotAllowed { index } => {
                        ISpendingUnitV2::MulticallMethodNotAllowed { index }.abi_encode()
                    }
                    ZeroAttesterRegistry => unreachable!(),
                };
                PrecompileError::RevertBytes(Bytes::from(encoded))
            }
        }
    }
}
