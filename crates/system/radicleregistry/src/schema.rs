use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::RADICLE_REGISTRY_ADDRESS;

/// Approved decision awaits its separate offchain execution.
pub const PATCH_EXECUTION_PENDING: u8 = 1;

/// Immutable decision. Execution and merge progress are stored separately.
#[derive(Clone, Debug, PartialEq, Eq)]
#[storage_record(exists_field = approved_height)]
pub struct PatchApprovalRecord {
    #[key]
    pub proposal_id: U256,
    #[attribute(order = 0)]
    pub payload: String,
    #[attribute(order = 1)]
    pub approved_height: u64,
    #[attribute(order = 2)]
    pub approved_timestamp: u64,
}

/// Consensus storage layout for the V1 Radicle repository registry.
///
/// Slots:
/// - 0 repository_count
/// - 1 repository_by_index
/// - 2 repository_index (index + 1)
/// - 3 repository_registrant
/// - 4 registry_generation
/// - 5 config_max_repositories
/// - 6..=8 patch_approvals (immutable canonical payload, height, timestamp)
/// - 9 patch_execution_stage (separate from immutable approval)
#[storage_schema]
#[contract(addr = RADICLE_REGISTRY_ADDRESS)]
pub struct RadicleRegistry {
    #[attribute(order = 0)]
    pub repository_count: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 1)]
    pub repository_by_index: outbe_primitives::storage::dsl::Map<u32, B256>,
    #[attribute(order = 2)]
    pub repository_index: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 3)]
    pub repository_registrant: outbe_primitives::storage::dsl::Map<B256, Address>,
    #[attribute(order = 4)]
    pub registry_generation: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 5)]
    pub config_max_repositories: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 6)]
    pub patch_approvals: outbe_primitives::storage::dsl::Map<U256, PatchApprovalRecord>,
    #[attribute(order = 7)]
    pub patch_execution_stage: outbe_primitives::storage::dsl::Map<U256, u8>,
}
