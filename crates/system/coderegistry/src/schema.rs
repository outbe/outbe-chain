use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::CODE_REGISTRY_ADDRESS;
use outbe_primitives::storage::types::StorageBytes;

pub use outbe_primitives::code_registry::{
    PatchApprovalRecord, PatchApprovalRecordEntryExt, PublicationReceiptRecord,
    PublicationReceiptRecordEntryExt, PATCH_EXECUTION_MERGED, PATCH_EXECUTION_PENDING,
    PATCH_EXECUTION_REVIEWED,
};

/// Consensus storage layout for the V1 CodeRegistry.
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
/// - 10 config_descriptor_hash (immutable genesis commitment)
/// - 11 config_descriptor_bytes (immutable Solidity bytes OGD1 artifact)
/// - 12 network_repository (immutable RID binding)
/// - 13..=15 publication sequence, previous receipt hash, material receipt sequence
/// - 16..=18 immutable publication receipts
/// - 19..=20 fixed pending slots keyed only by participant index
#[storage_schema]
#[contract(addr = CODE_REGISTRY_ADDRESS)]
pub struct CodeRegistry {
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
    #[attribute(order = 8)]
    pub config_descriptor_hash: outbe_primitives::storage::dsl::Value<B256>,
    #[attribute(order = 9)]
    pub config_descriptor_bytes: StorageBytes,
    #[attribute(order = 10)]
    pub network_repository: outbe_primitives::storage::dsl::Value<B256>,
    #[attribute(order = 11)]
    pub publication_sequence: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 12)]
    pub publication_receipt_hash: outbe_primitives::storage::dsl::Value<B256>,
    #[attribute(order = 13)]
    pub publication_material_sequence: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 14)]
    pub publication_receipts: outbe_primitives::storage::dsl::Map<u64, PublicationReceiptRecord>,
    #[attribute(order = 15)]
    pub publication_pending_generation: outbe_primitives::storage::dsl::Map<u32, B256>,
    #[attribute(order = 16)]
    pub publication_pending_body_hash: outbe_primitives::storage::dsl::Map<u32, B256>,
}
