//! Storage schema for the credisfactory precompile.
//!
//! Persists only the gratis pledge handle each position needs so `anadosis` can
//! address the right pledge record. The pledger EOA is NOT stored — the caller
//! passes it as an `eoaAccount` calldata arg at `anadosis`, and the enclave checks
//! it against the record — so the durable position state no longer carries the
//! EOA↔bundle linkage. Collateral is escrowed in the `CREDIS_ADDRESS` balance
//! between `requestCredis` and `anadosis`.
//!
//! The credis schedule itself (positions, anadosis installments) lives in the
//! `outbe_credis` crate; this schema only bridges `requestCredis` → `anadosis`.

use alloy_primitives::{B256, U256};
use outbe_macros::contract;
use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;
use outbe_primitives::storage::types::Mapping;

/// EVM storage layout for the credisfactory precompile.
///
/// Storage slots:
///   0: mapping(position_id => bytes32) — the gratis pledge handle
#[contract(addr = CREDIS_FACTORY_ADDRESS)]
pub struct CredisFactoryContract {
    pub position_pledge_handle: Mapping<U256, B256>,
}
