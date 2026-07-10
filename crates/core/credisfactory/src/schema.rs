//! Storage schema for the credisfactory precompile.
//!
//! Persists the confidential-pledge linkage each position needs so `anadosis`
//! can release the right pledger's collateral: the pledge handle (which addresses
//! the gratis pledge record) and the pledger EOA (whose encrypted balance the
//! unlock credits). The EOA↔bundle linkage is visible on-chain by the accepted
//! TEE-migration tradeoff — amounts stay encrypted (see the gratis
//! `apply_unlock_to_eoa` TODO for restoring unlinkability).
//!
//! The credis schedule itself (positions, anadosis installments) lives in the
//! `outbe_credis` crate; this schema only bridges `requestCredis` → `anadosis`.

use alloy_primitives::{Address, B256, U256};
use outbe_macros::contract;
use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;
use outbe_primitives::storage::types::Mapping;

/// EVM storage layout for the credisfactory precompile.
///
/// Storage slots:
///   0: mapping(position_id => bytes32) — the gratis pledge handle
///   1: mapping(position_id => address) — the original pledger EOA
#[contract(addr = CREDIS_FACTORY_ADDRESS)]
pub struct CredisFactoryContract {
    pub position_pledge_handle: Mapping<U256, B256>,
    pub position_pledger: Mapping<U256, Address>,
}
