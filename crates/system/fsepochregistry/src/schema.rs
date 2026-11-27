//! Persistent EVM storage schema for the FsEpochRegistry precompile.

use alloy_primitives::{Address, B256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::storage::dsl::Bytes as StorageBytes;
use outbe_system_common::addresses::FS_EPOCH_REGISTRY_ADDRESS;

/// EVM storage layout for `FsEpochRegistry` (`0x5200000000000000000000000000000000000003`).
///
/// Storage slots:
///   0: `admin` (`Address`) — genesis seeder writes slot 0 directly
///   1: `pending_admin` (`Address`)
///   2: `head` (`u32`)
///   3: `anchored` (`bool`)
///   4: `admin_next` (`bool`)
///   5: `epoch_pk_att` (`Map<u32, StorageBytes>`)
///   6: `epoch_dst` (`Map<u32, StorageBytes>`)
///   7: `epoch_start_wwd` (`Map<u32, u32>`)
///   8: `epoch_members_hash` (`Map<u32, B256>`)
///   9: `epoch_prev_hash` (`Map<u32, B256>`)
#[storage_schema]
#[contract(addr = FS_EPOCH_REGISTRY_ADDRESS)]
pub struct FsEpochRegistryContract {
    #[attribute(order = 0)]
    pub admin: Value<Address>,

    #[attribute(order = 1)]
    pub pending_admin: Value<Address>,

    #[attribute(order = 2)]
    pub head: Value<u32>,

    #[attribute(order = 3)]
    pub anchored: Value<bool>,

    #[attribute(order = 4)]
    pub admin_next: Value<bool>,

    #[attribute(order = 5)]
    pub epoch_pk_att: Map<u32, StorageBytes>,

    #[attribute(order = 6)]
    pub epoch_dst: Map<u32, StorageBytes>,

    #[attribute(order = 7)]
    pub epoch_start_wwd: Map<u32, u32>,

    #[attribute(order = 8)]
    pub epoch_members_hash: Map<u32, B256>,

    #[attribute(order = 9)]
    pub epoch_prev_hash: Map<u32, B256>,
}
