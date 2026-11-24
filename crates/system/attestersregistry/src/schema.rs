//! Persistent EVM storage schema for the AttestersRegistry precompile.

use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_system_common::addresses::ATTESTERS_REGISTRY_ADDRESS;

/// On-chain record for one registered attester.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[storage_record(exists_field = registered_at)]
pub struct AttesterRecord {
    #[key]
    pub attester: Address,

    #[attribute(order = 0)]
    pub active: bool,

    #[attribute(order = 1)]
    pub permission_mask: u32,

    #[attribute(order = 2)]
    pub registered_at: u64,

    #[attribute(order = 3)]
    pub is_rotation_candidate: bool,

    #[attribute(order = 4)]
    pub consensus_key: B256,

    #[attribute(order = 5)]
    pub p2p_addr: U256,
}

/// EVM storage layout for `AttestersRegistry` (`0x5200000000000000000000000000000000000001`).
///
/// Storage slots:
///   0: `admin` (`Address`) — genesis seeder writes slot 0 directly
///   1: `pending_admin` (`Address`)
///   2: `reshare_generation` (`u64`)
///   3: `activated_generation` (`u64`)
///   4: `activated_output_hash` (`B256`)
///   5: `all_actors` (`List<Address>`)
///   6: `reshare_skip_dealers` (`List<Address>`)
///   7..13: `records` (`Map<Address, AttesterRecord>`, 6 slots)
///   13: `attested` (`Map<u64, Map<Address, bool>>`)
///   14: `attested_output_hash` (`Map<u64, Map<Address, B256>>`)
#[storage_schema]
#[contract(addr = ATTESTERS_REGISTRY_ADDRESS)]
pub struct AttestersRegistryContract {
    #[attribute(order = 0)]
    pub admin: Value<Address>,

    #[attribute(order = 1)]
    pub pending_admin: Value<Address>,

    #[attribute(order = 2)]
    pub reshare_generation: Value<u64>,

    #[attribute(order = 3)]
    pub activated_generation: Value<u64>,

    #[attribute(order = 4)]
    pub activated_output_hash: Value<B256>,

    #[attribute(order = 5)]
    pub all_actors: List<Address>,

    #[attribute(order = 6)]
    pub reshare_skip_dealers: List<Address>,

    #[attribute(order = 7)]
    pub records: Map<Address, AttesterRecord>,

    #[attribute(order = 8)]
    pub attested: Map<u64, Map<Address, bool>>,

    #[attribute(order = 9)]
    pub attested_output_hash: Map<u64, Map<Address, B256>>,
}
