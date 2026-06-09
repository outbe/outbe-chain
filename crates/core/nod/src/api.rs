//! Cross-module API for the Nod entity store.
//!
//! NodFactory (and other callers) use these free functions to mutate the
//! Nod entity collections without reaching into [`crate::state`] directly.
//! Each function constructs a short-lived [`NodContract`] facade from the
//! handed-in `StorageHandle` and delegates to the same `state.rs` helpers
//! the in-crate runtime uses.

use alloy_primitives::{B256, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::{NodBucketState, NodContract, NodItemState};

/// Insert `item` into every Nod slot collection and bump bucket + supply.
///
/// `cost_of_gratis_minor` is consumed only when the bucket is created on this
/// call; it is not stored on `NodItemState` so the caller passes it
/// explicitly. The caller (NodFactory) validates inputs and asserts non-
/// existence of `item.nod_id` before invoking this function.
pub fn add_nod(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    entry_price_minor: U256,
) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    nod.add_nod(item, entry_price_minor)
}

/// Remove `item` from every Nod slot collection and decrement bucket + supply.
///
/// The caller has already loaded `item` (via [`get_item`]) and verified
/// authorization and business preconditions (owner, unlock, qualification).
pub fn remove_nod(storage: &StorageHandle<'_>, item: &NodItemState) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    nod.remove_nod(item)
}

/// Fetch a Nod item state by id.
pub fn get_item(storage: &StorageHandle<'_>, nod_id: U256) -> Result<Option<NodItemState>> {
    let nod = NodContract::new(storage.clone());
    nod.get_item(nod_id)
}

/// Fetch a Nod bucket state by bucket key.
pub fn get_bucket(storage: &StorageHandle<'_>, bucket_key: B256) -> Result<Option<NodBucketState>> {
    let nod = NodContract::new(storage.clone());
    nod.get_bucket(bucket_key)
}
