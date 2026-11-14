use crate::errors::VoteError;
use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::storage::{Storable, StorableType};
use outbe_system_common::addresses::VOTE_ADDRESS;

pub use outbe_system_common::vote_target::{
    read_proposal_status, read_vote_proposal, vote_proposals, ProposalRecord,
    ProposalRecordEntryExt, ProposalRecordView, ProposalStatus, VOTE_PROPOSALS_BASE_SLOT,
};

/// State of the native bond of a proposal.
///
/// Zero is deliberately `NoBond` so proposal records written before bond
/// accounting remain valid when the appended fields read as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BondState {
    NoBond = 0,
    Escrowed = 1,
    Refunded = 2,
    Burned = 3,
}

impl BondState {
    const VARIANTS: [Self; 4] = [Self::NoBond, Self::Escrowed, Self::Refunded, Self::Burned];

    pub fn from_u8(value: u8) -> std::result::Result<Self, VoteError> {
        decode_stored_u8(value, &Self::VARIANTS, Self::to_u8).ok_or(VoteError::CorruptBondState)
    }

    pub const fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Finds the variant with this stored byte. Variant order does not set its value.
pub(crate) fn decode_stored_u8<T: Copy>(
    value: u8,
    variants: &[T],
    stored: fn(T) -> u8,
) -> Option<T> {
    variants
        .iter()
        .copied()
        .find(|variant| stored(*variant) == value)
}

/// Vote record stored in each proposal's ordered vote list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteRecord {
    pub voter: Address,
    pub vote_kind: u8, // VoteKind
    /// Height of the latest accepted ballot, including a Yes-to-No change.
    pub block_number: u64,
}

impl StorableType for VoteRecord {
    const SLOTS: usize = 1;
}

impl Storable for VoteRecord {
    fn from_word(word: U256) -> Self {
        let bytes = word.to_be_bytes::<32>();
        let mut block_number = [0u8; 8];
        block_number.copy_from_slice(&bytes[21..29]);
        Self {
            voter: Address::from_slice(&bytes[..20]),
            vote_kind: bytes[20],
            block_number: u64::from_be_bytes(block_number),
        }
    }

    fn to_word(&self) -> U256 {
        let mut bytes = [0u8; 32];
        bytes[..20].copy_from_slice(self.voter.as_slice());
        bytes[20] = self.vote_kind;
        bytes[21..29].copy_from_slice(&self.block_number.to_be_bytes());
        U256::from_be_bytes(bytes)
    }
}

/// EVM storage layout for the generic Vote precompile.
///
/// Storage slots:
///   0:  proposal_count
///   1:  pending_proposal_ids
///   2..=7: proposals: mapping(proposal_id => ProposalRecord) (`ProposalRecord`, 6 slots)
///   8:  votes_map: mapping(voteKey => 1-based proposal_voters index)
///   9:  proposal_voters: mapping(proposalId => VoteRecord[])
///   10: proposal_bond_amount: mapping(proposalId => amount)
///   11: proposal_bond_state: mapping(proposalId => BondState)
///   12: escrowed_bond_total
///   13: proposal_electorate: mapping(proposalId => TargetElectorate)
#[storage_schema]
#[contract(addr = VOTE_ADDRESS)]
pub struct Vote {
    /// Total number of proposals ever created.
    #[attribute(order = 0)]
    pub proposal_count: outbe_primitives::storage::dsl::Value<U256>,

    /// Bounded list of proposal ids that are not terminal.
    /// A new `Error` id leaves this list. `Error` is terminal.
    /// `process_begin_block` drains a legacy `Error` id from an older binary.
    #[attribute(order = 1)]
    pub pending_proposal_ids: outbe_primitives::storage::dsl::List<U256>,

    #[attribute(order = 2)]
    pub proposals: outbe_primitives::storage::dsl::Map<U256, ProposalRecord>,

    #[attribute(order = 3)]
    /// Maps `keccak256(proposal_id || voter)` to a 1-based position in `proposal_voters`.
    /// Zero means the validator has not voted on the proposal.
    pub votes_map: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Ordered vote records per proposal.
    #[attribute(order = 4)]
    pub proposal_voters:
        outbe_primitives::storage::dsl::Map<U256, outbe_primitives::storage::dsl::List<VoteRecord>>,

    /// Appended native bond amount keyed by proposal id.
    #[attribute(order = 5)]
    pub proposal_bond_amount: outbe_primitives::storage::dsl::Map<U256, U256>,

    /// Appended `BondState` byte keyed by proposal id.
    #[attribute(order = 6)]
    pub proposal_bond_state: outbe_primitives::storage::dsl::Map<U256, u8>,

    /// Total amount of all proposal bonds whose state is `Escrowed`.
    #[attribute(order = 7)]
    pub escrowed_bond_total: outbe_primitives::storage::dsl::Value<U256>,

    /// Immutable target policy; absent legacy values select ACTIVE-only voting.
    #[attribute(order = 8)]
    pub proposal_electorate: outbe_primitives::storage::dsl::Map<U256, u8>,
}
