//! Shared vote-target interface, proposal record/status types, and handler registry.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolError};

use crate::addresses::VOTE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::dsl::{Map, RecordEntry, StorageRecord};
use outbe_primitives::storage::types::{Mapping, Slot, Storable, StorableType, StorageBytes};
use outbe_primitives::storage::StorageHandle;

sol! {
    error InvalidProposalStatus();
    error InvalidElectorate();
    error InvalidPayload();
    error UnknownTargetModule();
    error DuplicateTargetModule();
    error StoredPayloadInvalid(string module);
}

/// Shared vote-target and proposal-decoding errors with `IVote`-compatible Solidity custom error encoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoteTargetError {
    #[error("invalid proposal status")]
    InvalidProposalStatus,
    #[error("invalid stored proposal electorate")]
    InvalidElectorate,
    #[error("invalid proposal payload")]
    InvalidPayload,
    #[error("unknown vote target module")]
    UnknownTargetModule,
    #[error("duplicate vote target module handler")]
    DuplicateTargetModule,
    #[error("stored {module} proposal payload is invalid")]
    StoredPayloadInvalid { module: String },
}

impl From<VoteTargetError> for PrecompileError {
    fn from(error: VoteTargetError) -> Self {
        match error {
            VoteTargetError::InvalidProposalStatus => {
                PrecompileError::RevertBytes(Bytes::from(InvalidProposalStatus {}.abi_encode()))
            }
            VoteTargetError::InvalidElectorate => {
                PrecompileError::RevertBytes(Bytes::from(InvalidElectorate {}.abi_encode()))
            }
            VoteTargetError::InvalidPayload => {
                PrecompileError::RevertBytes(Bytes::from(InvalidPayload {}.abi_encode()))
            }
            VoteTargetError::UnknownTargetModule => {
                PrecompileError::RevertBytes(Bytes::from(UnknownTargetModule {}.abi_encode()))
            }
            VoteTargetError::DuplicateTargetModule => {
                PrecompileError::RevertBytes(Bytes::from(DuplicateTargetModule {}.abi_encode()))
            }
            VoteTargetError::StoredPayloadInvalid { module } => PrecompileError::RevertBytes(
                Bytes::from(StoredPayloadInvalid { module }.abi_encode()),
            ),
        }
    }
}

impl VoteTargetError {
    /// Creates a precompile error for an invalid stored proposal payload.
    #[must_use]
    pub fn stored_payload_invalid(module: &str) -> PrecompileError {
        Self::StoredPayloadInvalid {
            module: module.to_string(),
        }
        .into()
    }
}

/// Lifecycle status of a generic proposal.
///
/// Stored discriminants stay stable and intentionally differ from the compact ABI
/// enum. Convert at the precompile boundary instead of casting between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProposalStatus {
    Pending = 0,
    Approved = 1,
    Expired = 3,
    Error = 4,
}

impl ProposalStatus {
    pub const fn from_u8(value: u8) -> std::result::Result<Self, VoteTargetError> {
        match value {
            0 => Ok(Self::Pending),
            1 => Ok(Self::Approved),
            3 => Ok(Self::Expired),
            4 => Ok(Self::Error),
            _ => Err(VoteTargetError::InvalidProposalStatus),
        }
    }

    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Returns `true` when the proposal leaves the bounded pending index.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Approved | Self::Expired | Self::Error)
    }
}

/// Alias for [`ProposalRecord`] when used as a read-only storage view by target crates.
pub type ProposalRecordView = ProposalRecord;

/// Generic proposal record keyed by `id` at `VOTE_ADDRESS` base slot `2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRecord {
    pub id: U256,
    pub proposer: Address,
    pub target_module: Address,
    pub payload: String,
    pub created_height: u64,
    pub voting_deadline_height: u64,
    pub status: u8,
}

impl ProposalRecord {
    #[must_use]
    pub fn with_key(key: U256) -> Self {
        Self {
            id: key,
            proposer: Address::default(),
            target_module: Address::default(),
            payload: String::default(),
            created_height: 0,
            voting_deadline_height: 0,
            status: 0,
        }
    }

    /// Reads the typed proposal status from storage.
    pub fn proposal_status(&self) -> std::result::Result<ProposalStatus, VoteTargetError> {
        ProposalStatus::from_u8(self.status)
    }

    /// Writes the typed proposal status to storage.
    pub fn set_proposal_status(&mut self, status: ProposalStatus) {
        self.status = status.to_u8();
    }
}

impl StorableType for ProposalRecord {
    const SLOTS: usize = 6;
}

impl StorageRecord for ProposalRecord {
    type Key = U256;
    const SLOTS: u64 = <Self as StorableType>::SLOTS as u64;

    fn key(&self) -> Self::Key {
        self.id
    }

    fn exists(entry: &RecordEntry<'_, Self::Key, Self>) -> Result<bool> {
        let value = Mapping::<U256, Address>::new(
            entry.base_slot() + U256::from(0u64),
            entry.address(),
            entry.storage(),
        )
        .read(entry.key_ref())?;
        Ok(!<Address as Storable>::to_word(&value).is_zero())
    }

    fn load(entry: &RecordEntry<'_, Self::Key, Self>) -> Result<Option<Self>> {
        if !Self::exists(entry)? {
            return Ok(None);
        }
        Ok(Some(Self {
            proposer: Mapping::<U256, Address>::new(
                entry.base_slot() + U256::from(0u64),
                entry.address(),
                entry.storage(),
            )
            .read(entry.key_ref())?,
            target_module: Mapping::<U256, Address>::new(
                entry.base_slot() + U256::from(1u64),
                entry.address(),
                entry.storage(),
            )
            .read(entry.key_ref())?,
            payload: Mapping::<U256, StorageBytes>::new(
                entry.base_slot() + U256::from(2u64),
                entry.address(),
                entry.storage(),
            )
            .read_string(entry.key_ref())?,
            created_height: Mapping::<U256, u64>::new(
                entry.base_slot() + U256::from(3u64),
                entry.address(),
                entry.storage(),
            )
            .read(entry.key_ref())?,
            voting_deadline_height: Mapping::<U256, u64>::new(
                entry.base_slot() + U256::from(4u64),
                entry.address(),
                entry.storage(),
            )
            .read(entry.key_ref())?,
            status: Mapping::<U256, u8>::new(
                entry.base_slot() + U256::from(5u64),
                entry.address(),
                entry.storage(),
            )
            .read(entry.key_ref())?,
            id: entry.key(),
        }))
    }

    fn create(entry: &RecordEntry<'_, Self::Key, Self>, value: &Self) -> Result<()> {
        if Self::exists(entry)? {
            return Err(outbe_primitives::storage::dsl::existing_record_err(
                "ProposalRecord",
            ));
        }
        Self::write_fields(entry, value)
    }

    fn update(entry: &RecordEntry<'_, Self::Key, Self>, value: &Self) -> Result<()> {
        if !Self::exists(entry)? {
            return Err(outbe_primitives::storage::dsl::missing_record_err(
                "ProposalRecord",
            ));
        }
        Self::write_fields(entry, value)
    }

    fn delete(entry: &RecordEntry<'_, Self::Key, Self>) -> Result<()> {
        Mapping::<U256, Address>::new(
            entry.base_slot() + U256::from(0u64),
            entry.address(),
            entry.storage(),
        )
        .get(entry.key_ref())
        .delete()?;
        Mapping::<U256, Address>::new(
            entry.base_slot() + U256::from(1u64),
            entry.address(),
            entry.storage(),
        )
        .get(entry.key_ref())
        .delete()?;
        Mapping::<U256, StorageBytes>::new(
            entry.base_slot() + U256::from(2u64),
            entry.address(),
            entry.storage(),
        )
        .get_bytes(entry.key_ref())
        .clear()?;
        Mapping::<U256, u64>::new(
            entry.base_slot() + U256::from(3u64),
            entry.address(),
            entry.storage(),
        )
        .get(entry.key_ref())
        .delete()?;
        Mapping::<U256, u64>::new(
            entry.base_slot() + U256::from(4u64),
            entry.address(),
            entry.storage(),
        )
        .get(entry.key_ref())
        .delete()?;
        Mapping::<U256, u8>::new(
            entry.base_slot() + U256::from(5u64),
            entry.address(),
            entry.storage(),
        )
        .get(entry.key_ref())
        .delete()?;
        Ok(())
    }
}

impl ProposalRecord {
    fn write_fields(entry: &RecordEntry<'_, U256, Self>, value: &Self) -> Result<()> {
        Mapping::<U256, Address>::new(
            entry.base_slot() + U256::from(0u64),
            entry.address(),
            entry.storage(),
        )
        .write(entry.key_ref(), value.proposer)?;
        Mapping::<U256, Address>::new(
            entry.base_slot() + U256::from(1u64),
            entry.address(),
            entry.storage(),
        )
        .write(entry.key_ref(), value.target_module)?;
        Mapping::<U256, StorageBytes>::new(
            entry.base_slot() + U256::from(2u64),
            entry.address(),
            entry.storage(),
        )
        .get_bytes(entry.key_ref())
        .write_if_changed(value.payload.as_bytes())?;
        Mapping::<U256, u64>::new(
            entry.base_slot() + U256::from(3u64),
            entry.address(),
            entry.storage(),
        )
        .write(entry.key_ref(), value.created_height)?;
        Mapping::<U256, u64>::new(
            entry.base_slot() + U256::from(4u64),
            entry.address(),
            entry.storage(),
        )
        .write(entry.key_ref(), value.voting_deadline_height)?;
        Mapping::<U256, u8>::new(
            entry.base_slot() + U256::from(5u64),
            entry.address(),
            entry.storage(),
        )
        .write(entry.key_ref(), value.status)?;
        Ok(())
    }
}

pub trait ProposalRecordEntryExt<'storage> {
    fn proposer(&self) -> Slot<'storage, Address>;
    fn target_module(&self) -> Slot<'storage, Address>;
    fn payload(&self) -> StorageBytes<'storage>;
    fn created_height(&self) -> Slot<'storage, u64>;
    fn voting_deadline_height(&self) -> Slot<'storage, u64>;
    fn status(&self) -> Slot<'storage, u8>;
}

impl<'storage> ProposalRecordEntryExt<'storage> for RecordEntry<'storage, U256, ProposalRecord> {
    fn proposer(&self) -> Slot<'storage, Address> {
        Mapping::<U256, Address>::new(
            self.base_slot() + U256::from(0u64),
            self.address(),
            self.storage(),
        )
        .get(self.key_ref())
    }

    fn target_module(&self) -> Slot<'storage, Address> {
        Mapping::<U256, Address>::new(
            self.base_slot() + U256::from(1u64),
            self.address(),
            self.storage(),
        )
        .get(self.key_ref())
    }

    fn payload(&self) -> StorageBytes<'storage> {
        Mapping::<U256, StorageBytes>::new(
            self.base_slot() + U256::from(2u64),
            self.address(),
            self.storage(),
        )
        .get_bytes(self.key_ref())
    }

    fn created_height(&self) -> Slot<'storage, u64> {
        Mapping::<U256, u64>::new(
            self.base_slot() + U256::from(3u64),
            self.address(),
            self.storage(),
        )
        .get(self.key_ref())
    }

    fn voting_deadline_height(&self) -> Slot<'storage, u64> {
        Mapping::<U256, u64>::new(
            self.base_slot() + U256::from(4u64),
            self.address(),
            self.storage(),
        )
        .get(self.key_ref())
    }

    fn status(&self) -> Slot<'storage, u8> {
        Mapping::<U256, u8>::new(
            self.base_slot() + U256::from(5u64),
            self.address(),
            self.storage(),
        )
        .get(self.key_ref())
    }
}

/// Base storage slot of `proposals: Map<U256, ProposalRecord>` in `Vote` (`VOTE_ADDRESS`).
pub const VOTE_PROPOSALS_BASE_SLOT: u64 = 2;

/// Returns a storage map handle to `Vote.proposals` at `VOTE_ADDRESS`.
#[must_use]
pub fn vote_proposals(storage: StorageHandle<'_>) -> Map<'_, U256, ProposalRecord> {
    Map::new(U256::from(VOTE_PROPOSALS_BASE_SLOT), VOTE_ADDRESS, storage)
}

/// Loads a `ProposalRecord` by `proposal_id` directly from `VOTE_ADDRESS` storage.
pub fn read_vote_proposal(
    storage: StorageHandle<'_>,
    proposal_id: U256,
) -> Result<Option<ProposalRecord>> {
    vote_proposals(storage).get(proposal_id)
}

/// Loads the typed [`ProposalStatus`] of `proposal_id` directly from `VOTE_ADDRESS` storage.
pub fn read_proposal_status(
    storage: StorageHandle<'_>,
    proposal_id: U256,
) -> Result<Option<ProposalStatus>> {
    let Some(record) = read_vote_proposal(storage, proposal_id)? else {
        return Ok(None);
    };
    Ok(Some(record.proposal_status()?))
}

/// Static handler table entry type.
pub type VoteTargetHandlers = &'static [&'static dyn VoteTarget];

/// Compile-time proposal admission class owned by a target module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAdmission {
    ActiveValidatorOnly,
    PublicBonded { amount: U256 },
}

/// Electorate policy selected by the target and persisted at proposal admission.
/// Zero preserves the ACTIVE-only policy of existing proposals and targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TargetElectorate {
    Active = 0,
    ActiveConsensus = 1,
}

impl TargetElectorate {
    pub fn from_stored(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Active),
            1 => Ok(Self::ActiveConsensus),
            _ => Err(VoteTargetError::InvalidElectorate.into()),
        }
    }
}

/// Consensus context passed explicitly to target validation and execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoteTargetContext {
    pub proposer: Address,
    pub attached_value: U256,
    pub block_number: u64,
    pub chain_id: u64,
}

impl VoteTargetContext {
    /// Builds the target execution context for a tallied `proposal`.
    pub fn for_proposal(
        ctx: &BlockRuntimeContext,
        proposal: &ProposalRecord,
        attached_value: U256,
    ) -> Result<Self> {
        Ok(Self {
            proposer: proposal.proposer,
            attached_value,
            block_number: ctx.block.block_number,
            chain_id: ctx.storage.chain_id()?,
        })
    }
}

/// Deterministic result of applying an approved proposal to its target module.
///
/// Infrastructure/provider failures remain the outer [`Result::Err`] and abort
/// execution. Only a target-declared proposal execution failure uses [`Self::Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetExecutionOutcome {
    Applied,
    Error { reason: String },
}

/// Decodes a JSON proposal payload for admission.
/// Reverts malformed JSON with [`VoteTargetError::InvalidPayload`].
pub fn decode_proposal_payload(payload: &[u8]) -> Result<serde_json::Value> {
    serde_json::from_slice(payload).map_err(|_| VoteTargetError::InvalidPayload.into())
}

/// Decodes the stored JSON payload of an approved proposal for `module`.
/// Returns a precompile error if the stored payload is malformed JSON.
pub fn decode_stored_proposal_payload(payload: &[u8], module: &str) -> Result<serde_json::Value> {
    serde_json::from_slice(payload).map_err(|_| VoteTargetError::stored_payload_invalid(module))
}

/// Target-module handler for approved vote proposals.
pub trait VoteTarget: Send + Sync {
    /// Precompile address this handler serves.
    fn target_module(&self) -> Address;

    /// Fail-fast validation used during proposal creation.
    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()>;

    /// Applies side effects when a proposal is approved.
    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome>;

    /// Compile-time admission class. V1 targets default to validator-only, zero value.
    fn admission(&self) -> TargetAdmission {
        TargetAdmission::ActiveValidatorOnly
    }

    /// Controls both cast eligibility and the exact tally denominator.
    fn electorate(&self) -> TargetElectorate {
        TargetElectorate::Active
    }

    /// Atomically reserves target-owned admission state for the allocated proposal id.
    fn reserve(
        &self,
        _storage: StorageHandle<'_>,
        _proposal_id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<()> {
        Ok(())
    }

    /// Dispatches terminal proposal outcomes to the target module.
    /// Only a tally result is possible (Expired or Approved).
    fn handle_tally(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
        status: ProposalStatus,
    ) -> Result<TargetExecutionOutcome> {
        if status == ProposalStatus::Approved {
            return self.handle_approved(ctx, proposal_id, payload, context);
        }
        Ok(TargetExecutionOutcome::Applied)
    }
}

/// Read-only view over a compile-time handler table.
#[derive(Clone, Copy)]
pub struct VoteTargetRegistry {
    handlers: VoteTargetHandlers,
}

impl VoteTargetRegistry {
    /// Builds a registry from a static handler table.
    #[must_use]
    pub const fn new(handlers: VoteTargetHandlers) -> Self {
        Self { handlers }
    }

    /// Returns the handler registered for `target_module`, if any.
    ///
    /// Returns an error when more than one handler is registered for the same address.
    pub fn lookup(&self, target_module: Address) -> Result<&'static dyn VoteTarget> {
        let mut matches = self
            .handlers
            .iter()
            .filter(|handler| handler.target_module() == target_module);
        let Some(first) = matches.next() else {
            return Err(VoteTargetError::UnknownTargetModule.into());
        };
        if matches.next().is_some() {
            return Err(VoteTargetError::DuplicateTargetModule.into());
        }
        Ok(*first)
    }

    /// Dispatches a terminal proposal outcome to the target module of `proposal`.
    pub fn handle_tally(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        proposal: &ProposalRecord,
        attached_value: U256,
        status: ProposalStatus,
    ) -> Result<TargetExecutionOutcome> {
        let target = self.lookup(proposal.target_module)?;
        let context = VoteTargetContext::for_proposal(ctx, proposal, attached_value)?;
        target.handle_tally(
            ctx,
            proposal_id,
            proposal.payload.as_bytes(),
            context,
            status,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    #[test]
    fn proposal_status_discriminants_and_terminal_flags_are_preserved() {
        assert_eq!(ProposalStatus::from_u8(0), Ok(ProposalStatus::Pending));
        assert_eq!(ProposalStatus::from_u8(1), Ok(ProposalStatus::Approved));
        assert_eq!(
            ProposalStatus::from_u8(2),
            Err(VoteTargetError::InvalidProposalStatus)
        );
        assert_eq!(ProposalStatus::from_u8(3), Ok(ProposalStatus::Expired));
        assert_eq!(ProposalStatus::from_u8(4), Ok(ProposalStatus::Error));
        assert!(!ProposalStatus::Pending.is_terminal());
        assert!(ProposalStatus::Approved.is_terminal());
        assert!(ProposalStatus::Expired.is_terminal());
        assert!(ProposalStatus::Error.is_terminal());
    }

    #[test]
    fn proposal_record_round_trips_through_vote_storage() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let id = U256::from(7u64);
            let record = ProposalRecord {
                id,
                proposer: Address::repeat_byte(0x11),
                target_module: Address::repeat_byte(0x22),
                payload: r#"{"op":"test"}"#.to_string(),
                created_height: 10,
                voting_deadline_height: 20,
                status: ProposalStatus::Approved.to_u8(),
            };
            vote_proposals(storage.clone()).create(&record).unwrap();
            let loaded: ProposalRecordView =
                read_vote_proposal(storage.clone(), id).unwrap().unwrap();
            assert_eq!(loaded, record);
            assert_eq!(loaded.proposal_status(), Ok(ProposalStatus::Approved));
            assert_eq!(
                read_proposal_status(storage.clone(), id).unwrap(),
                Some(ProposalStatus::Approved)
            );
            assert_eq!(
                read_proposal_status(storage, U256::from(999u64)).unwrap(),
                None
            );
        });
    }
}
