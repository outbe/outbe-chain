//! Error types for the GemFactory precompile.

use crate::precompile::IGemFactory;
use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolError;
use outbe_common::pow::PowError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the GemFactory runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemFactoryError {
    #[error("gem not found")]
    GemNotFound,

    #[error("gem is already settled")]
    GemAlreadySettled,

    #[error("gem is not settled")]
    GemNotSettled,

    #[error("gem group is not called")]
    GemNotCalled,

    #[error("settlement deadline has passed")]
    SettlementDeadlinePassed,

    #[error("source intex not found")]
    SourceIntexNotFound,

    #[error("source intex is not issued")]
    SourceIntexNotIssued,

    #[error("position not found")]
    PositionNotFound,

    #[error("only position owner can issue merchant gem")]
    NotPositionOwner,

    #[error("position already exists")]
    PositionAlreadyExists,

    #[error("index out of bounds")]
    IndexOutOfBounds,

    #[error("insufficient factory capacity")]
    InsufficientCapacity,

    #[error("position expired")]
    PositionExpired,

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("{currency} is not an ISO 4217 currency code")]
    InvalidCurrency { currency: u16 },

    #[error("oracle nominal unavailable")]
    OracleUnavailable,

    #[error("invalid owner")]
    InvalidOwner,

    #[error("overflow")]
    Overflow,

    #[error("promis load must be positive")]
    ZeroPromisLoad,

    #[error("gem group {group_key} has an empty member slot during cleanup")]
    EmptyMemberSlotDuringCleanup { group_key: B256 },

    #[error("gem group {group_key} member {gem_id} has no record during cleanup")]
    MissingMemberRecordDuringCleanup { group_key: B256, gem_id: U256 },

    #[error("position has not expired")]
    PositionNotExpired,

    #[error("gem group {group} has no record to call")]
    MissingGroupForCall { group: B256 },

    #[error("gem group {group_key} indexes an ineligible member {gem_id}")]
    IneligibleGroupMember { group_key: B256, gem_id: U256 },

    #[error("gem call price overflow")]
    CallPriceOverflow,

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => GemFactoryError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<GemFactoryError> for PrecompileError {
    fn from(value: GemFactoryError) -> Self {
        use IGemFactory as I;
        match value {
            GemFactoryError::GemNotFound => revert_with(I::GemNotFound {}),
            GemFactoryError::GemAlreadySettled => revert_with(I::GemAlreadySettled {}),
            GemFactoryError::GemNotSettled => revert_with(I::GemNotSettled {}),
            GemFactoryError::GemNotCalled => revert_with(I::GemNotCalled {}),
            GemFactoryError::SettlementDeadlinePassed => {
                revert_with(I::SettlementDeadlinePassed {})
            }
            GemFactoryError::SourceIntexNotFound => revert_with(I::SourceIntexNotFound {}),
            GemFactoryError::SourceIntexNotIssued => revert_with(I::SourceIntexNotIssued {}),
            GemFactoryError::PositionNotFound => revert_with(I::PositionNotFound {}),
            GemFactoryError::NotPositionOwner => revert_with(I::NotPositionOwner {}),
            GemFactoryError::PositionAlreadyExists => revert_with(I::PositionAlreadyExists {}),
            GemFactoryError::IndexOutOfBounds => revert_with(I::IndexOutOfBounds {}),
            GemFactoryError::InsufficientCapacity => revert_with(I::InsufficientCapacity {}),
            GemFactoryError::PositionExpired => revert_with(I::PositionExpired {}),
            GemFactoryError::InsufficientProofOfWork => revert_with(I::InsufficientProofOfWork {}),
            GemFactoryError::InvalidCurrency { currency } => {
                revert_with(I::InvalidCurrency { currency })
            }
            GemFactoryError::OracleUnavailable => revert_with(I::OracleUnavailable {}),
            GemFactoryError::InvalidOwner => revert_with(I::InvalidOwner {}),
            GemFactoryError::Overflow => revert_with(I::Overflow {}),
            GemFactoryError::ZeroPromisLoad => revert_with(I::ZeroPromisLoad {}),
            GemFactoryError::EmptyMemberSlotDuringCleanup { group_key } => {
                revert_with(I::EmptyMemberSlotDuringCleanup {
                    groupKey: group_key,
                })
            }
            GemFactoryError::MissingMemberRecordDuringCleanup { group_key, gem_id } => {
                revert_with(I::MissingMemberRecordDuringCleanup {
                    groupKey: group_key,
                    gemId: gem_id,
                })
            }
            GemFactoryError::PositionNotExpired => revert_with(I::PositionNotExpired {}),
            GemFactoryError::MissingGroupForCall { group } => {
                revert_with(I::MissingGroupForCall { group })
            }
            GemFactoryError::IneligibleGroupMember { group_key, gem_id } => {
                revert_with(I::IneligibleGroupMember {
                    groupKey: group_key,
                    gemId: gem_id,
                })
            }
            GemFactoryError::CallPriceOverflow => revert_with(I::CallPriceOverflow {}),
            GemFactoryError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}

impl From<PowError> for GemFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
