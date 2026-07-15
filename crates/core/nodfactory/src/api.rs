//! Cross-module API for NodFactory.
//!
//! Lysis calls [`issue_nod_with_reader`] inside its lysis run. The production
//! ABI surface (only `mineGratis`) lives in [`crate::precompile`] and requires
//! the same explicit body-read authority.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_compressed_entities::EntityId36;
use outbe_nod::{schema::NodIssueParams, NodRepositoryReader};

use crate::runtime;

/// Issue a new Nod with the explicit off-chain body reader.
pub fn issue_nod_with_reader(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
    params: &NodIssueParams,
) -> Result<EntityId36> {
    runtime::issue_nod_with_reader(storage, reader, params)
}

/// Mine a Nod with the explicit off-chain body reader.
pub fn mine_gratis_with_reader(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
    caller: Address,
    nod_id: EntityId36,
    nonce: U256,
    asset: Address,
) -> Result<U256> {
    runtime::mine_gratis_with_reader(storage, reader, caller, nod_id, nonce, asset)
}
