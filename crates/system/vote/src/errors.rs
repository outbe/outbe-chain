use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;

use crate::abi::IVote;

/// Vote module errors.
#[derive(Debug, thiserror::Error)]
pub enum VoteError {
    #[error("caller is not an active validator")]
    NotValidator,
    #[error("proposal not found")]
    ProposalNotFound,
    #[error("proposal is not pending")]
    NotPending,
    #[error("proposal voting window is closed")]
    VotingClosed,
    #[error("voting window must be in 1..={maximum} blocks, got {actual}")]
    InvalidVotingWindow { actual: u64, maximum: u64 },
    #[error("proposal voting deadline overflows block height")]
    VotingDeadlineOverflow,
    #[error("validator has already voted on proposal")]
    AlreadyVoted,
    #[error("too many pending proposals")]
    TooManyPending,
    #[error("validator has too many pending proposals")]
    TooManyPendingByValidator,
    #[error("too many pending public bonded proposals")]
    TooManyPendingPublicBonded,
    #[error("proposer already has a pending public bonded proposal")]
    TooManyPendingPublicBondedByProposer,
    #[error("invalid proposal status")]
    InvalidProposalStatus,
    #[error("invalid vote kind")]
    InvalidVoteKind,
    #[error("invalid stored proposal electorate")]
    InvalidElectorate,
    #[error("invalid proposal bond settlement")]
    InvalidBondSettlement,
    #[error("proposal counter is exhausted")]
    ProposalCounterExhausted,
    #[error("proposal bond amount must be nonzero")]
    InvalidBondAmount,
    #[error("proposal bond is already recorded")]
    ProposalBondAlreadyRecorded,
    #[error("proposal bond is not unsettled")]
    ProposalBondNotUnsettled,
    #[error("proposal bond liability overflow")]
    BondLiabilityOverflow,
    #[error("proposal bond liability underflow")]
    BondLiabilityUnderflow,
    #[error("invalid proposal bond: expected {expected}, got {actual}")]
    InvalidProposalBond { expected: U256, actual: U256 },
    #[error("proposal bond liabilities {liabilities} exceed Vote balance {balance}")]
    BondLiabilityInvariant { balance: U256, liabilities: U256 },
    #[error("invalid proposal payload")]
    InvalidPayload,
    #[error("unknown vote target module")]
    UnknownTargetModule,
    #[error("duplicate vote target module handler")]
    DuplicateTargetModule,
    #[error("stored {module} proposal payload is invalid")]
    StoredPayloadInvalid { module: String },
    #[error("pending proposal {proposal_id} has an already settled bond")]
    PendingProposalBondAlreadySettled {
        proposal_id: U256,
        settlement: crate::schema::BondSettlement,
    },
    #[error("proposal {proposal_id} has inconsistent bond amount and settlement")]
    InconsistentBondState { proposal_id: U256 },
}

impl From<VoteError> for PrecompileError {
    fn from(error: VoteError) -> Self {
        match error {
            VoteError::InvalidProposalBond { expected, actual } => PrecompileError::RevertBytes(
                Bytes::from(IVote::InvalidProposalBond { expected, actual }.abi_encode()),
            ),
            VoteError::BondLiabilityInvariant {
                balance,
                liabilities,
            } => PrecompileError::RevertBytes(Bytes::from(
                IVote::ProposalBondLiabilityInvariant {
                    balance,
                    liabilities,
                }
                .abi_encode(),
            )),
            VoteError::PendingProposalBondAlreadySettled {
                proposal_id,
                settlement,
            } => {
                let settlement = match settlement {
                    crate::schema::BondSettlement::NoBond => IVote::BondSettlement::NoBond,
                    crate::schema::BondSettlement::Unsettled => IVote::BondSettlement::Unsettled,
                    crate::schema::BondSettlement::Refunded => IVote::BondSettlement::Refunded,
                    crate::schema::BondSettlement::Burned => IVote::BondSettlement::Burned,
                };
                PrecompileError::RevertBytes(Bytes::from(
                    IVote::ProposalBondAlreadySettled {
                        proposalId: proposal_id,
                        settlement,
                    }
                    .abi_encode(),
                ))
            }
            VoteError::NotValidator => {
                PrecompileError::RevertBytes(Bytes::from(IVote::NotValidator {}.abi_encode()))
            }
            VoteError::ProposalNotFound => {
                PrecompileError::RevertBytes(Bytes::from(IVote::ProposalNotFound {}.abi_encode()))
            }
            VoteError::NotPending => {
                PrecompileError::RevertBytes(Bytes::from(IVote::NotPending {}.abi_encode()))
            }
            VoteError::VotingClosed => {
                PrecompileError::RevertBytes(Bytes::from(IVote::VotingClosed {}.abi_encode()))
            }
            VoteError::InvalidVotingWindow { actual, maximum } => PrecompileError::RevertBytes(
                Bytes::from(IVote::InvalidVotingWindow { actual, maximum }.abi_encode()),
            ),
            VoteError::VotingDeadlineOverflow => PrecompileError::RevertBytes(Bytes::from(
                IVote::VotingDeadlineOverflow {}.abi_encode(),
            )),
            VoteError::AlreadyVoted => {
                PrecompileError::RevertBytes(Bytes::from(IVote::AlreadyVoted {}.abi_encode()))
            }
            VoteError::TooManyPending => {
                PrecompileError::RevertBytes(Bytes::from(IVote::TooManyPending {}.abi_encode()))
            }
            VoteError::TooManyPendingByValidator => PrecompileError::RevertBytes(Bytes::from(
                IVote::TooManyPendingByValidator {}.abi_encode(),
            )),
            VoteError::TooManyPendingPublicBonded => PrecompileError::RevertBytes(Bytes::from(
                IVote::TooManyPendingPublicBonded {}.abi_encode(),
            )),
            VoteError::TooManyPendingPublicBondedByProposer => PrecompileError::RevertBytes(
                Bytes::from(IVote::TooManyPendingPublicBondedByProposer {}.abi_encode()),
            ),
            VoteError::InvalidProposalStatus => PrecompileError::RevertBytes(Bytes::from(
                IVote::InvalidProposalStatus {}.abi_encode(),
            )),
            VoteError::InvalidVoteKind => {
                PrecompileError::RevertBytes(Bytes::from(IVote::InvalidVoteKind {}.abi_encode()))
            }
            VoteError::InvalidElectorate => {
                PrecompileError::RevertBytes(Bytes::from(IVote::InvalidElectorate {}.abi_encode()))
            }
            VoteError::InvalidBondSettlement => PrecompileError::RevertBytes(Bytes::from(
                IVote::InvalidBondSettlement {}.abi_encode(),
            )),
            VoteError::ProposalCounterExhausted => PrecompileError::RevertBytes(Bytes::from(
                IVote::ProposalCounterExhausted {}.abi_encode(),
            )),
            VoteError::InvalidBondAmount => {
                PrecompileError::RevertBytes(Bytes::from(IVote::InvalidBondAmount {}.abi_encode()))
            }
            VoteError::ProposalBondAlreadyRecorded => PrecompileError::RevertBytes(Bytes::from(
                IVote::ProposalBondAlreadyRecorded {}.abi_encode(),
            )),
            VoteError::ProposalBondNotUnsettled => PrecompileError::RevertBytes(Bytes::from(
                IVote::ProposalBondNotUnsettled {}.abi_encode(),
            )),
            VoteError::BondLiabilityOverflow => PrecompileError::RevertBytes(Bytes::from(
                IVote::BondLiabilityOverflow {}.abi_encode(),
            )),
            VoteError::BondLiabilityUnderflow => PrecompileError::RevertBytes(Bytes::from(
                IVote::BondLiabilityUnderflow {}.abi_encode(),
            )),
            VoteError::InvalidPayload => {
                PrecompileError::RevertBytes(Bytes::from(IVote::InvalidPayload {}.abi_encode()))
            }
            VoteError::UnknownTargetModule => PrecompileError::RevertBytes(Bytes::from(
                IVote::UnknownTargetModule {}.abi_encode(),
            )),
            VoteError::DuplicateTargetModule => PrecompileError::RevertBytes(Bytes::from(
                IVote::DuplicateTargetModule {}.abi_encode(),
            )),
            VoteError::StoredPayloadInvalid { module } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IVote::StoredPayloadInvalid {
                        module: module.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            VoteError::InconsistentBondState { proposal_id } => PrecompileError::RevertBytes(
                Bytes::from(IVote::InconsistentBondState { proposal_id }.abi_encode()),
            ),
        }
    }
}

impl VoteError {
    /// Creates a fatal precompile error for an invalid stored proposal payload.
    pub fn stored_payload_invalid(module: &str) -> PrecompileError {
        Self::StoredPayloadInvalid {
            module: module.to_string(),
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(err: VoteError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn bond_settlement_and_liability_fields_use_the_declared_errors() {
        let proposal_id = U256::from(7u64);
        let bytes = encoded(VoteError::PendingProposalBondAlreadySettled {
            proposal_id,
            settlement: crate::schema::BondSettlement::Refunded,
        });
        let decoded =
            IVote::ProposalBondAlreadySettled::abi_decode(&bytes).expect("settlement payload");
        assert_eq!(decoded.proposalId, proposal_id);
        assert_eq!(decoded.settlement, IVote::BondSettlement::Refunded);

        let balance = U256::from(3u64);
        let liabilities = U256::from(9u64);
        let bytes = encoded(VoteError::BondLiabilityInvariant {
            balance,
            liabilities,
        });
        let decoded =
            IVote::ProposalBondLiabilityInvariant::abi_decode(&bytes).expect("liability payload");
        assert_eq!(decoded.balance, balance);
        assert_eq!(decoded.liabilities, liabilities);

        let bytes = encoded(VoteError::StoredPayloadInvalid {
            module: "Update".to_string(),
        });
        let decoded = IVote::StoredPayloadInvalid::abi_decode(&bytes).expect("module payload");
        assert_eq!(decoded.module, "Update");
    }
}
