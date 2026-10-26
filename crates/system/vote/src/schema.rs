use crate::errors::VoteError;
use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::VOTE_ADDRESS;
use outbe_primitives::storage::{Storable, StorableType};

/// Lifecycle status of a generic proposal.
///
/// Stored discriminants stay stable and intentionally differ from the compact ABI
/// enum. Convert at the precompile boundary instead of casting between them.
/// The removed reserved status is invalid rather than reused: old records must
/// never acquire a different meaning when the public enum is compacted.
///
/// Voting has two outcomes after the inclusive `voting_deadline_height`:
/// - Quorum reached: dispatch to the target; successful application is `Approved`.
/// - Quorum not reached: the proposal is `Expired`.
///
/// `Pending` is intermediate. A target-declared execution failure is `Error`,
/// not a rejection by the voters. It is terminal, leaves the pending index and
/// refunds its bond in the same pass; the module does not retry it.
///
/// There is no manual cancellation or separate conflict-rejection transition.
/// Further state, such as a scheduled upgrade, belongs to the target module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProposalStatus {
    Pending = 0,
    Approved = 1,
    Expired = 3,
    Error = 4,
}

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

impl ProposalStatus {
    const VARIANTS: [Self; 4] = [Self::Pending, Self::Approved, Self::Expired, Self::Error];

    pub fn from_u8(value: u8) -> std::result::Result<Self, VoteError> {
        decode_stored_u8(value, &Self::VARIANTS, Self::to_u8)
            .ok_or(VoteError::InvalidProposalStatus)
    }

    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Returns `true` when the proposal leaves the bounded pending index.
    /// `Error` is terminal. The module never retries a target-execution failure.
    /// It refunds the bond when it records the `Error` status.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Approved | Self::Expired | Self::Error)
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

/// Generic proposal record keyed by `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = proposer)]
pub struct ProposalRecord {
    #[key]
    pub id: U256,

    #[attribute(order = 0)]
    pub proposer: Address,

    #[attribute(order = 1)]
    pub target_module: Address,

    #[attribute(order = 2)]
    pub payload: String,

    #[attribute(order = 3)]
    pub created_height: u64,

    #[attribute(order = 4)]
    pub voting_deadline_height: u64,

    // TODO: Extend storage to support enum fields?
    #[attribute(order = 5)]
    pub status: u8, // ProposalStatus
}

impl ProposalRecord {
    /// Reads the typed proposal status from storage.
    pub fn proposal_status(&self) -> std::result::Result<ProposalStatus, VoteError> {
        ProposalStatus::from_u8(self.status)
    }

    /// Writes the typed proposal status to storage.
    pub fn set_proposal_status(&mut self, status: ProposalStatus) {
        self.status = status.to_u8();
    }
}

/// Vote record stored in each proposal's ordered vote list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteRecord {
    pub voter: Address,
    pub vote_kind: u8, // VoteKind
    pub block_number: u64,
}

// TODO: Can we modify `#[storage_record]` macro to generate `Storable`?
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
