//! Storage schema for the CCA registry.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::CCA_REGISTRY_ADDRESS;
use outbe_primitives::storage::types::StorageKey;

use crate::errors::CcaError;

/// Registration state of a CCA.
///
/// `Suspended` is reserved for the §8.1 bond top-up grace lapsing; nothing sets
/// it yet, but it is a distinct condition from deregistering — a suspended CCA
/// is expected back, a deregistered one is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CcaState {
    Active = 0,
    Suspended = 1,
    Deregistered = 2,
}

impl CcaState {
    pub fn from_u8(value: u8) -> Result<Self, CcaError> {
        match value {
            0 => Ok(Self::Active),
            1 => Ok(Self::Suspended),
            2 => Ok(Self::Deregistered),
            other => Err(CcaError::InvalidStateValue(other)),
        }
    }
}

/// One registered Credis Card Agent.
///
/// The bond fields of §8.1/§8.2 are deliberately absent: the bond is a separate
/// subsystem (dynamic requirement, delegated layers, per-layer cooldowns, exit
/// haircut) and adding it later is an append to this record, not a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = registered)]
pub struct CcaRecord {
    #[key]
    pub cca: Address,

    /// Presence flag. An explicit marker rather than overloading a value field:
    /// `state` is zero for `Active`, and `multiplier` can in principle decay to
    /// zero under enough voids, so neither can stand in for existence.
    #[attribute(order = 4)]
    pub registered: bool,

    /// `m` — the §8.4 performance multiplier, 1e18 scaled. Starts at 1 and only
    /// ever moves down on a void and back up on repayment; it is never above 1.
    #[attribute(order = 0)]
    pub multiplier: U256,

    #[attribute(order = 1)]
    pub registered_at: u64,

    /// Registration state as `u8`; decode via [`CcaState::from_u8`].
    #[attribute(order = 2)]
    pub state: u8,

    /// Settled principal counted toward the next recovery step but not yet large
    /// enough to earn one. Carrying the remainder here is what stops integer
    /// division from silently discarding repayment.
    #[attribute(order = 3)]
    pub recovery_progress: U256,
}

impl CcaRecord {
    pub fn registration_state(&self) -> Result<CcaState, CcaError> {
        CcaState::from_u8(self.state)
    }
}

/// EVM storage layout for the CCA registry precompile.
///
/// The per-day columns are what the daily reward distribution reads: it needs
/// both "how many units did this CCA earn" and "which CCAs earned any", so the
/// units map is paired with a dense per-day index in the same shape as
/// `AgentRewardContract`'s WAA/SRA day lists.
#[storage_schema]
#[contract(addr = CCA_REGISTRY_ADDRESS)]
pub struct CcaContract {
    /// The registry itself, keyed by agent address.
    #[attribute(order = 0)]
    pub ccas: outbe_primitives::storage::dsl::Map<Address, CcaRecord>,

    /// Origination units earned per day, 1e18 scaled —
    /// `keccak(day ++ cca) -> units`.
    #[attribute(order = 1)]
    pub day_units: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// How many distinct CCAs earned units on a day.
    #[attribute(order = 2)]
    pub day_cca_count: outbe_primitives::storage::dsl::Map<WorldwideDay, u32>,

    /// Dense per-day CCA index — `keccak(day ++ index) -> cca`.
    #[attribute(order = 3)]
    pub day_ccas: outbe_primitives::storage::dsl::Map<B256, Address>,

    /// Lifetime count of positions one CCA has originated for one owner, which
    /// drives the §8.3 repeat-owner decay — `keccak(cca ++ owner) -> count`.
    #[attribute(order = 4)]
    pub owner_origination_counts: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Whether a CCA already earned its one unit for an owner on a day —
    /// `keccak(day ++ cca ++ owner) -> seen`.
    #[attribute(order = 5)]
    pub owner_day_seen: outbe_primitives::storage::dsl::Map<B256, bool>,
}

impl CcaContract<'_> {
    /// `keccak256(day ++ cca)` — per-day units for one agent.
    pub fn day_unit_key(day: WorldwideDay, cca: Address) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..4].copy_from_slice(day.key_bytes().as_slice());
        buf[4..24].copy_from_slice(cca.as_slice());
        keccak256(buf)
    }

    /// `keccak256(day ++ index_be32)` — dense per-day agent index.
    pub fn day_index_key(day: WorldwideDay, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(day.key_bytes().as_slice());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    /// `keccak256(cca ++ owner)` — lifetime repeat-owner counter.
    pub fn owner_key(cca: Address, owner: Address) -> B256 {
        let mut buf = [0u8; 40];
        buf[0..20].copy_from_slice(cca.as_slice());
        buf[20..40].copy_from_slice(owner.as_slice());
        keccak256(buf)
    }

    /// `keccak256(day ++ cca ++ owner)` — one-unit-per-owner-per-day guard.
    pub fn owner_day_key(day: WorldwideDay, cca: Address, owner: Address) -> B256 {
        let mut buf = [0u8; 44];
        buf[0..4].copy_from_slice(day.key_bytes().as_slice());
        buf[4..24].copy_from_slice(cca.as_slice());
        buf[24..44].copy_from_slice(owner.as_slice());
        keccak256(buf)
    }
}
