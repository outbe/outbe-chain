use alloy_primitives::{B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::storage::types::Mapping;

use crate::errors::UpdateError;
use crate::ProtocolVersion;

pub use outbe_system_common::scheduled_update::ScheduledUpdateStatus;

/// Scheduled update record keyed by vote `proposal_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = version)]
pub struct ScheduledUpdateRecord {
    #[key]
    pub proposal_id: U256,

    #[attribute(order = 0)]
    pub version: ProtocolVersion,

    #[attribute(order = 1)]
    pub activation_height: u64,

    #[attribute(order = 2)]
    pub info: String,

    #[attribute(order = 3)]
    pub status: u8, // ScheduledUpdateStatus
}

impl ScheduledUpdateRecord {
    pub fn scheduled_update_status(
        &self,
    ) -> std::result::Result<ScheduledUpdateStatus, UpdateError> {
        ScheduledUpdateStatus::from_u8(self.status)
            .map_err(|_| UpdateError::InvalidScheduledUpdateStatus)
    }

    pub fn set_scheduled_update_status(&mut self, status: ScheduledUpdateStatus) {
        self.status = status.to_u8();
    }
}

/// EVM storage layout for the Update precompile.
#[storage_schema]
#[contract(addr = UPDATE_ADDRESS)]
pub struct Update {
    #[attribute(order = 0)]
    pub active_version: outbe_primitives::storage::dsl::Value<ProtocolVersion>,

    #[attribute(order = 1)]
    pub active_version_height: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 2)]
    pub waiting_for_activation_proposal_ids: outbe_primitives::storage::dsl::List<U256>,

    #[attribute(order = 3)]
    pub scheduled_updates: outbe_primitives::storage::dsl::Map<U256, ScheduledUpdateRecord>,

    #[attribute(order = 4)]
    pub version_history: Mapping<u64, ProtocolVersion>,

    /// Proposal id => keccak256 of the approved `upgradeContracts` payload.
    /// Zero = none or already applied.
    #[attribute(order = 5)]
    pub approved_upgrades: outbe_primitives::storage::dsl::Map<U256, B256>,

    /// Proposal id => block of the approval.
    #[attribute(order = 6)]
    pub approved_upgrade_at: outbe_primitives::storage::dsl::Map<U256, u64>,
}
