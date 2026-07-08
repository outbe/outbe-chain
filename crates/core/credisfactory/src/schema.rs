//! Storage schema for the credisfactory precompile.
//!
//! The factory persists only the per-position **denomination index** set at
//! `requestCredis`. Each `anadosis` call reads it to derive the anadosis
//! (one-decade-down) denomination and insert the user-supplied per-installment
//! reclaim commitment into the gratispool's matching Merkle tree (see
//! `outbe_gratispool::api::add_commitment`).
//!
//! The credis schedule itself (positions, anadosis installments) lives in
//! the `outbe_credis` crate; this schema only persists what the factory
//! needs to bridge `requestCredis` → `anadosis` for the shielded flow.

use alloy_primitives::U256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;

/// EVM storage layout for the credisfactory precompile.
#[storage_schema]
#[contract(addr = CREDIS_FACTORY_ADDRESS)]
pub struct CredisFactoryContract {
    /// slot 0: per-position denomination id (widened from `u8` to `u32` for
    /// the storage primitive's `StorageKey` requirements). Keyed by
    /// `position_id`. Read on every `anadosis` to derive the anadosis
    /// (one-decade-down) denomination for the reclaim-note insert.
    #[attribute(order = 0)]
    pub position_denom: outbe_primitives::storage::dsl::Map<U256, u32>,
}
