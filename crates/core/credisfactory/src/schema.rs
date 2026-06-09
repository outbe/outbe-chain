//! Storage schema for the credisfactory precompile.
//!
//! The factory holds per-position **reclaim metadata** — the denomination
//! index and the reclaim commitment the user supplied at `requestCredis`.
//! When the position completes through `anadosis`, the factory looks the
//! pair up and inserts the reclaim commitment back into the gratispool's
//! per-denomination Merkle tree (see `outbe_gratispool::api::insert_reclaim`).
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
    /// `position_id`.
    #[attribute(order = 0)]
    pub position_denom: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// slot 1: per-position reclaim commitment. Inserted into the gratispool
    /// when the position's final anadosis is paid. Keyed by `position_id`.
    #[attribute(order = 1)]
    pub position_reclaim_commitment: outbe_primitives::storage::dsl::Map<U256, U256>,
}
