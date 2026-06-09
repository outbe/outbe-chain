//! Storage schema for the Credis contract.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::CREDIS_ADDRESS;

/// Number of monthly anadosis installments per position (cosmos `NumberOfPayments`).
pub const NUMBER_OF_ANADOSIS: u32 = 10;

/// Seconds per month (30 days; cosmos `SecondsPerMonth`).
pub const SECONDS_PER_MONTH: u64 = 30 * 24 * 60 * 60;

/// Position head record. Keyed by `position_id = keccak256(commitment || bundle_account)`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = bundle_account)]
pub struct Position {
    #[key]
    pub position_id: U256,

    #[attribute(order = 0)]
    pub bundle_account: Address,

    #[attribute(order = 1)]
    pub vault_provider: Address,

    #[attribute(order = 2)]
    pub asset: Address,

    #[attribute(order = 3)]
    pub total_anadosis_amount: U256,

    #[attribute(order = 4)]
    pub outstanding_anadosis_amount: U256,

    #[attribute(order = 5)]
    pub total_gratis_amount: U256,

    #[attribute(order = 6)]
    pub outstanding_gratis_amount: U256,

    #[attribute(order = 7)]
    pub next_anadosis_number: u32,

    #[attribute(order = 8)]
    pub created_at: u64,
}

/// Per-anadosis record. Keyed by `anadosis_key = keccak256(position_id || anadosis_number_be32)`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = due_date)]
pub struct Anadosis {
    #[key]
    pub anadosis_key: B256,

    #[attribute(order = 0)]
    pub anadosis_number: u32,

    #[attribute(order = 1)]
    pub due_date: u64,

    #[attribute(order = 2)]
    pub anadosis_amount: U256,

    #[attribute(order = 3)]
    pub gratis_amount: U256,

    #[attribute(order = 4, default = 0)]
    pub paid_at: u64,
}

/// EVM storage layout for the Credis position contract.
///
/// - positions / anadosis_records are keyed by `position_id` and `anadosis_key`
///   respectively.
/// - `address_position_*` and `total_positions` / `position_id_at_index`
///   provide dense, no-`Vec` enumeration in the same shape as `outbe-nod`'s
///   owner index (`crates/core/nod/src/schema.rs:107-119`).
#[storage_schema]
#[contract(addr = CREDIS_ADDRESS)]
pub struct CredisContract {
    /// slots 0..8: position head record keyed by position_id (9 slots).
    #[attribute(order = 0)]
    pub positions: outbe_primitives::storage::dsl::Map<U256, Position>,

    /// slots 9..13: anadosis record keyed by anadosis_key (5 slots).
    #[attribute(order = 1)]
    pub anadosis_records: outbe_primitives::storage::dsl::Map<B256, Anadosis>,

    /// slot 14: per-account count of positions ever created.
    #[attribute(order = 2)]
    pub address_position_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    /// slot 15: per-account index — keccak(addr ++ idx_be32) → position_id.
    #[attribute(order = 3)]
    pub address_position_ids: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// slot 16: total positions ever created (for getAllPositions iteration).
    #[attribute(order = 4)]
    pub total_positions: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 17: dense index — index → position_id.
    #[attribute(order = 5)]
    pub position_id_at_index: outbe_primitives::storage::dsl::Map<u64, U256>,
}

impl CredisContract<'_> {
    /// position_id derivation: `keccak256(commitment || bundle_account)`.
    pub fn position_id(commitment: U256, bundle_account: Address) -> U256 {
        let mut buf = [0u8; 52];
        buf[0..32].copy_from_slice(&commitment.to_be_bytes::<32>());
        buf[32..52].copy_from_slice(bundle_account.as_slice());
        U256::from_be_bytes(keccak256(buf).0)
    }

    /// Composite key for per-anadosis storage: `keccak256(position_id || anadosis_number_be32)`.
    pub fn anadosis_key(position_id: U256, anadosis_number: u32) -> B256 {
        let mut buf = [0u8; 36];
        buf[0..32].copy_from_slice(&position_id.to_be_bytes::<32>());
        buf[32..36].copy_from_slice(&anadosis_number.to_be_bytes());
        keccak256(buf)
    }

    /// Composite key for per-address position index: `keccak256(addr ++ idx_be32)`.
    pub fn address_index_key(account: Address, index: u32) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..20].copy_from_slice(account.as_slice());
        buf[20..24].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}
