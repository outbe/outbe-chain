//! Typed error definitions for the StablecoinFactory precompile.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::abi::IStablecoinFactory;

/// Represents errors returned by the StablecoinFactory runtime.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum StablecoinFactoryError {
    #[error("unexpected native value {value}")]
    UnexpectedValue { value: U256 },

    #[error("invalid issuer {issuer}")]
    InvalidIssuer { issuer: Address },

    #[error("invalid ticker {ticker}")]
    InvalidTicker { ticker: String },

    #[error("non-canonical stablecoin proposal payload")]
    NonCanonicalPayload,

    #[error("unknown stablecoin policy {policy_id}")]
    UnknownPolicy { policy_id: U256 },

    #[error("token id {token_id} is reserved by proposal {proposal_id}")]
    TokenIdReserved { token_id: B256, proposal_id: U256 },

    #[error("ticker {ticker} is reserved by proposal {proposal_id}")]
    TickerReserved { ticker: String, proposal_id: U256 },

    #[error("token id {token_id} is already registered at {token}")]
    TokenIdAlreadyRegistered { token_id: B256, token: Address },

    #[error("ticker {ticker} is already registered at {token}")]
    TickerAlreadyRegistered { ticker: String, token: Address },

    #[error(
        "token address {token} belongs to {existing_token_id}, not requested id {requested_token_id}"
    )]
    TokenAddressCollision {
        token: Address,
        existing_token_id: B256,
        requested_token_id: B256,
    },

    #[error("list limit {limit} must be between 1 and {maximum}")]
    InvalidListLimit { limit: U256, maximum: usize },

    #[error("unknown stablecoin {token}")]
    UnknownStablecoin { token: Address },

    #[error("stablecoin factory: token set length/data mismatch")]
    TokenSetLengthDataMismatch,

    #[error("stablecoin factory: proposal id zero cannot own a reservation")]
    ZeroProposalReservation,

    #[error("stablecoin factory: proposal {proposal_id} already owns a reservation")]
    ProposalAlreadyOwnsReservation { proposal_id: U256 },

    #[error(
        "stablecoin factory: proposal {proposal_id} payload does not match its Factory reservation"
    )]
    ReservationPayloadMismatch { proposal_id: U256 },

    #[error(
        "stablecoin factory: reserved stablecoin address {token} has pre-existing nonce or code"
    )]
    NonPristineTokenAccount { token: Address },

    #[error("stablecoin factory: reserved token address is already in the permanent token set")]
    ReservedTokenAlreadyInPermanentSet,

    #[error("stablecoin factory: proposal {proposal_id} has no Factory reservation")]
    MissingReservation { proposal_id: U256 },

    #[error(
        "stablecoin factory: proposal {proposal_id} reservation triple does not match its record"
    )]
    ReservationTripleMismatch { proposal_id: U256 },

    #[error("stablecoin factory: proposal {proposal_id} reservation collides with permanent Factory state")]
    ReservationCollidesWithPermanentState { proposal_id: U256 },

    #[error("stablecoin Factory schema {schema} is unsupported")]
    UnsupportedSchema { schema: u32 },
}

#[derive(Debug, Error)]
pub enum AbiDecodeError {
    #[error(transparent)]
    Sol(#[from] alloy_sol_types::Error),
    #[error("non-canonical ABI calldata or trailing bytes")]
    NonCanonicalCalldata,
}

impl From<StablecoinFactoryError> for PrecompileError {
    fn from(error: StablecoinFactoryError) -> Self {
        use IStablecoinFactory as I;

        let encoded = match error {
            StablecoinFactoryError::UnexpectedValue { value } => {
                I::UnexpectedValue { value }.abi_encode()
            }
            StablecoinFactoryError::InvalidIssuer { issuer } => {
                I::InvalidIssuer { issuer }.abi_encode()
            }
            StablecoinFactoryError::InvalidTicker { ticker } => {
                I::InvalidTicker { ticker }.abi_encode()
            }
            StablecoinFactoryError::NonCanonicalPayload => I::NonCanonicalPayload {}.abi_encode(),
            StablecoinFactoryError::UnknownPolicy { policy_id } => I::UnknownPolicy {
                policyId: policy_id,
            }
            .abi_encode(),
            StablecoinFactoryError::TokenIdReserved {
                token_id,
                proposal_id,
            } => I::TokenIdReserved {
                tokenId: token_id,
                proposalId: proposal_id,
            }
            .abi_encode(),
            StablecoinFactoryError::TickerReserved {
                ticker,
                proposal_id,
            } => I::TickerReserved {
                ticker,
                proposalId: proposal_id,
            }
            .abi_encode(),
            StablecoinFactoryError::TokenIdAlreadyRegistered { token_id, token } => {
                I::TokenIdAlreadyRegistered {
                    tokenId: token_id,
                    token,
                }
                .abi_encode()
            }
            StablecoinFactoryError::TickerAlreadyRegistered { ticker, token } => {
                I::TickerAlreadyRegistered { ticker, token }.abi_encode()
            }
            StablecoinFactoryError::TokenAddressCollision {
                token,
                existing_token_id,
                requested_token_id,
            } => I::TokenAddressCollision {
                token,
                existingTokenId: existing_token_id,
                requestedTokenId: requested_token_id,
            }
            .abi_encode(),
            StablecoinFactoryError::InvalidListLimit { limit, maximum } => I::InvalidListLimit {
                limit,
                maximum: U256::from(maximum),
            }
            .abi_encode(),
            StablecoinFactoryError::UnknownStablecoin { token } => {
                I::UnknownStablecoin { token }.abi_encode()
            }
            StablecoinFactoryError::TokenSetLengthDataMismatch
            | StablecoinFactoryError::ZeroProposalReservation
            | StablecoinFactoryError::ProposalAlreadyOwnsReservation { .. }
            | StablecoinFactoryError::ReservationPayloadMismatch { .. }
            | StablecoinFactoryError::NonPristineTokenAccount { .. }
            | StablecoinFactoryError::ReservedTokenAlreadyInPermanentSet
            | StablecoinFactoryError::MissingReservation { .. }
            | StablecoinFactoryError::ReservationTripleMismatch { .. }
            | StablecoinFactoryError::ReservationCollidesWithPermanentState { .. }
            | StablecoinFactoryError::UnsupportedSchema { .. } => {
                return PrecompileError::Revert(error.to_string());
            }
        };
        PrecompileError::RevertBytes(Bytes::from(encoded))
    }
}
