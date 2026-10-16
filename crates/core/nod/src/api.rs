//! Cross-module API for the Nod entity store.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::time::first_full_day;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::schema::{CallTerms, EffectiveState, NodBucketState, NodContract, NodItemState};

/// Derives the current entitlement state without waiting for sweep cleanup.
/// Paid entitlements survive expiry; the settlement deadline is inclusive.
#[must_use]
pub fn effective_state(
    item: &NodItemState,
    qualified: bool,
    called_at: u64,
    deadline: u64,
    now: U256,
) -> EffectiveState {
    if item.is_settled {
        EffectiveState::Settled
    } else if called_at != 0 && now > U256::from(deadline) {
        EffectiveState::Forfeited
    } else if called_at != 0 {
        EffectiveState::Called
    } else if qualified {
        EffectiveState::Qualified
    } else {
        EffectiveState::Issued
    }
}

pub fn is_qualified(storage: &StorageHandle<'_>, bucket: &NodBucketState) -> Result<bool> {
    let issued_at = NodContract::new(storage.clone())
        .bucket_issued_at
        .read(&bucket.bucket_key)?;
    outbe_oracle::api::closed_above_floor(
        storage.clone(),
        bucket.reference_currency,
        bucket.floor_price_minor()?,
        first_full_day(issued_at),
    )
}

/// The Nod's settlement cost: `floor(entry_price_minor * gratis_load_minor / 1e6)`.
/// Positive inputs have a minimum cost of one reference-currency minor unit.
/// Price and cost use six-decimal reference-currency precision. The load uses
/// protocol units (1e6 per whole COEN). Asset payment units are quoted separately.
///
/// The cost is derived rather than stored. The entry price lives on the Nod's
/// bucket and the load lives on the Nod itself. Lysis mints the Nod from exactly
/// this formula.
pub use outbe_primitives::nod_rules::settlement_cost_minor;

/// Timestamp by which a called bucket must be settled, or `0` while it is not
/// called at all.
///
/// Reads the notice period the bucket sealed at issuance, so retuning the
/// constant cannot move the deadline of a bucket that is already called.
pub fn settlement_deadline(storage: &StorageHandle<'_>, bucket_key: B256) -> Result<u64> {
    let nod = NodContract::new(storage.clone());
    let called_at = nod.bucket_called_at.read(&bucket_key)?;
    if called_at == 0 {
        return Ok(0);
    }
    let notice = nod.bucket_call_notice_period_seconds.read(&bucket_key)?;
    Ok(settlement_deadline_of(called_at, notice))
}

pub use outbe_primitives::settlement::settlement_deadline_of;

pub use crate::state_api::{LoadedNodBucket, LoadedNodItem};

/// Inserts a Nod item, increments membership and creates its bucket if absent atomically.
/// A new bucket seals `call_terms`; NodFactory derives them from the live profile.
pub fn add_nod(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    entry_price_minor: U256,
    call_terms: Option<CallTerms>,
) -> Result<()> {
    crate::state_api::add_item(storage, item, entry_price_minor, call_terms)
}

/// Removes a loaded Nod item and decrements membership, deleting the bucket only if empty.
pub fn remove_nod(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    bucket: LoadedNodBucket,
) -> Result<()> {
    crate::state_api::remove_item(storage, item, bucket)
}

/// Loads a Nod item while retaining its exact stored state for mutation.
pub fn load_item(
    storage: &StorageHandle<'_>,
    nod_id: WwdEntityId,
) -> Result<Option<LoadedNodItem>> {
    crate::state_api::load_item(storage, nod_id)
}

/// Loads a Nod bucket while retaining its exact stored state for mutation.
pub fn load_bucket(
    storage: &StorageHandle<'_>,
    bucket_key: B256,
) -> Result<Option<LoadedNodBucket>> {
    crate::state_api::load_bucket(storage, bucket_key)
}

/// Fetches a Nod item state by ID.
pub fn get_item(storage: &StorageHandle<'_>, nod_id: WwdEntityId) -> Result<Option<NodItemState>> {
    load_item(storage, nod_id).map(|loaded| loaded.map(|loaded| loaded.into_parts().0))
}

/// Fetches a Nod bucket state by ID.
pub fn get_bucket(storage: &StorageHandle<'_>, bucket_key: B256) -> Result<Option<NodBucketState>> {
    load_bucket(storage, bucket_key).map(|loaded| loaded.map(|loaded| loaded.into_parts().0))
}

/// Loads the complete Nod collection with overlay-correct membership.
pub fn list_all(storage: &StorageHandle<'_>) -> Result<Vec<NodItemState>> {
    NodContract::new(storage.clone())
        .state_records(None)
        .map(|records| records.into_iter().map(Into::into).collect())
}

/// Loads one owner's Nod collection with overlay-correct membership.
pub fn list_by_owner(storage: &StorageHandle<'_>, owner: Address) -> Result<Vec<NodItemState>> {
    NodContract::new(storage.clone())
        .state_records(Some(owner))
        .map(|records| records.into_iter().map(Into::into).collect())
}

/// Records payment without consuming the entitlement or changing live supply.
pub fn record_settled(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    bucket: LoadedNodBucket,
) -> Result<()> {
    crate::state_api::record_settled(storage, item, bucket)
}

/// Internal arithmetic boundary; public bodies and projections retain ciphertext.
pub fn calculation_amount(item: &NodItemState) -> Result<U256> {
    crate::enclave_client::read_amount(&item.encrypted)
        .map_err(|error| crate::errors::NodError::AmountReadFailed(error).into())
}
