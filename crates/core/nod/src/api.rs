//! Cross-module API for the Nod entity store.
//!
//! NodFactory (and other callers) use these free functions to mutate the
//! Nod entity collections without reaching into [`crate::state`] directly.
//! Each function constructs a short-lived [`NodContract`] facade from the
//! handed-in `StorageHandle` and delegates to the same `state.rs` helpers
//! the in-crate runtime uses.

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{CommitmentState, EntityId36};
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
pub fn get_item(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
    nod_id: EntityId36,
) -> Result<Option<NodItemState>> {
    NodContract::new(storage.clone()).get_item_verified(reader, nod_id)
}

/// Fetch a Nod bucket state by bucket key.
pub fn get_bucket(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
    bucket_id: EntityId36,
) -> Result<Option<NodBucketState>> {
    NodContract::new(storage.clone()).get_bucket_verified(reader, bucket_id)
}

/// Loads the full canonical Nod collection in ascending Nod ID order.
pub fn list_all(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
) -> Result<Vec<NodItemState>> {
    let records = collect_pages(|after| {
        reader.list_all(NodPageRequest {
            after,
            limit: MAX_SCAN_ENTRIES,
        })
    })?;
    verify_items(storage, records, "global")
}

/// Loads one owner's canonical Nod collection in ascending Nod ID order.
pub fn list_by_owner(
    storage: &StorageHandle<'_>,
    reader: &NodRepositoryReader,
    owner: Address,
) -> Result<Vec<NodItemState>> {
    let records = collect_pages(|after| {
        reader.list_by_owner(
            owner,
            NodPageRequest {
                after,
                limit: MAX_SCAN_ENTRIES,
            },
        )
    })?;
    verify_items(storage, records, "owner")
}

fn collect_pages(
    mut page: impl FnMut(
        Option<EntityId36>,
    ) -> std::result::Result<crate::NodPage, crate::NodRepositoryError>,
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

fn verify_items(
    storage: &StorageHandle<'_>,
    records: Vec<NodItemState>,
    index: &'static str,
) -> Result<Vec<NodItemState>> {
    let nod = NodContract::new(storage.clone());
    let commitments = CommitmentState::new(storage.clone());
    let mut verified = Vec::with_capacity(records.len());
    for body in records {
        let expected = commitments.nod_item(body.nod_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "{index} index returned canonically absent Nod {}",
                body.nod_id
            ))
        })?;
        nod.verify_item(&body, expected)?;
        if verified
            .last()
            .is_some_and(|last: &NodItemState| last.nod_id >= body.nod_id)
        {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod {index} page contains duplicate or unordered identities"
                )),
            );
        }
        verified.push(body);
    }
    Ok(verified)
}
