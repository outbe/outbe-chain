//! Cross-module API for the Nod entity store.
//!
//! NodFactory (and other callers) use these free functions to mutate the
//! Nod entity collections without reaching into [`crate::state`] directly.
//! Each function constructs a short-lived [`NodContract`] facade from the
//! handed-in `StorageHandle` and delegates to the same `state.rs` helpers
//! the in-crate runtime uses.

use alloy_primitives::{Address, B256, U256};
use outbe_offchain_storage::MAX_SCAN_ENTRIES;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::{NodBucketState, NodContract, NodItemState};
use crate::{errors::NodError, NodPageRequest, NodRepositoryReader};

const MAX_RUNTIME_QUERY_RECORDS: usize = MAX_SCAN_ENTRIES * 4;

/// Insert `item` into every Nod slot collection and bump bucket + supply.
///
/// `cost_of_gratis_minor` is consumed only when the bucket is created on this
/// call; it is not stored on `NodItemState` so the caller passes it
/// explicitly. The caller (NodFactory) validates inputs and asserts non-
/// existence of `item.nod_id` before invoking this function.
pub fn add_nod(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
    item: &NodItemState,
    entry_price_minor: U256,
) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    nod.record_nod_issued(reader, item, entry_price_minor)
}

/// Remove `item` from every Nod slot collection and decrement bucket + supply.
///
/// The caller has already loaded `item` (via [`get_item`]) and verified
/// authorization and business preconditions (owner, qualification).
pub fn remove_nod(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
    item: &NodItemState,
) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    nod.record_nod_removed(reader, item)
}

/// Fetch a Nod item state by id.
pub fn get_item(reader: &NodRepositoryReader, nod_id: U256) -> Result<Option<NodItemState>> {
    reader.get(nod_id).map_err(Into::into)
}

/// Fetch a Nod bucket state by bucket key.
pub fn get_bucket(
    reader: &NodRepositoryReader,
    bucket_key: B256,
) -> Result<Option<NodBucketState>> {
    reader.get_bucket(bucket_key).map_err(Into::into)
}

/// Loads the full canonical Nod collection in ascending Nod ID order.
pub fn list_all(reader: &NodRepositoryReader) -> Result<Vec<NodItemState>> {
    collect_pages(|after| {
        reader.list_all(NodPageRequest {
            after,
            limit: MAX_SCAN_ENTRIES,
        })
    })
}

/// Loads one owner's canonical Nod collection in ascending Nod ID order.
pub fn list_by_owner(reader: &NodRepositoryReader, owner: Address) -> Result<Vec<NodItemState>> {
    collect_pages(|after| {
        reader.list_by_owner(
            owner,
            NodPageRequest {
                after,
                limit: MAX_SCAN_ENTRIES,
            },
        )
    })
}

fn collect_pages(
    mut page: impl FnMut(Option<U256>) -> std::result::Result<crate::NodPage, crate::NodRepositoryError>,
) -> Result<Vec<NodItemState>> {
    let mut after = None;
    let mut records = Vec::new();
    loop {
        let next = page(after)?;
        if records.len().saturating_add(next.records.len()) > MAX_RUNTIME_QUERY_RECORDS {
            return Err(NodError::QueryLimitExceeded.into());
        }
        records.extend(next.records);
        match next.next_after {
            Some(_) if records.len() == MAX_RUNTIME_QUERY_RECORDS => {
                return Err(NodError::QueryLimitExceeded.into());
            }
            Some(cursor) => after = Some(cursor),
            None => return Ok(records),
        }
    }
}

/// Legacy EVM body fixture for tests that predate the off-chain read boundary.
#[cfg(any(test, feature = "test-utils"))]
pub fn fixture_add_nod(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    entry_price_minor: U256,
) -> Result<()> {
    NodContract::new(storage.clone()).add_nod(item, entry_price_minor)
}

/// Legacy EVM body fixture for tests that predate the off-chain read boundary.
#[cfg(any(test, feature = "test-utils"))]
pub fn fixture_remove_nod(storage: &StorageHandle<'_>, item: &NodItemState) -> Result<()> {
    NodContract::new(storage.clone()).remove_nod(item)
}

/// Legacy EVM body fixture for tests that predate the off-chain read boundary.
#[cfg(any(test, feature = "test-utils"))]
pub fn fixture_get_item(storage: &StorageHandle<'_>, nod_id: U256) -> Result<Option<NodItemState>> {
    NodContract::new(storage.clone()).get_item(nod_id)
}

/// Legacy EVM body fixture for tests that predate the off-chain read boundary.
#[cfg(any(test, feature = "test-utils"))]
pub fn fixture_get_bucket(
    storage: &StorageHandle<'_>,
    bucket_key: B256,
) -> Result<Option<NodBucketState>> {
    NodContract::new(storage.clone()).get_bucket(bucket_key)
}
