//! Cross-module API for the Nod entity store.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::schema::{CallTerms, EffectiveState, NodContract, NodGroupState, NodItemState};

/// Derives the current entitlement state without waiting for sweep cleanup.
/// Paid entitlements survive expiry; the settlement deadline is inclusive.
#[must_use]
pub fn effective_state(
    item: &NodItemState,
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
    } else {
        EffectiveState::Issued
    }
}

/// The Nod's settlement cost: `floor(entry_price_minor * gratis_load_minor / 1e6)`.
/// Positive inputs have a minimum cost of one reference-currency minor unit.
/// Price and cost use six-decimal reference-currency precision. The load uses
/// protocol units (1e6 per whole COEN). Asset payment units are quoted separately.
///
/// The cost is derived rather than stored from the entry price and the load the
/// Nod carries. Lysis mints the Nod from exactly this formula.
pub use outbe_primitives::nod_rules::settlement_cost_minor;

/// Timestamp by which a called group must be settled, or `0` while it is not
/// called at all.
///
/// Reads the notice period the group sealed at issuance, so retuning the
/// constant cannot move the deadline of a group that is already called.
pub fn settlement_deadline(storage: &StorageHandle<'_>, group_key: B256) -> Result<u64> {
    let nod = NodContract::new(storage.clone());
    let called_at = nod.group_called_at.read(&group_key)?;
    if called_at == 0 {
        return Ok(0);
    }
    let notice = nod.group_call_notice_period_seconds.read(&group_key)?;
    Ok(settlement_deadline_of(called_at, notice))
}

pub use outbe_primitives::settlement::settlement_deadline_of;

pub use crate::state_api::{LoadedNodGroup, LoadedNodItem};

/// Inserts a Nod item, increments membership and creates its group if absent atomically.
/// A new group seals `call_terms`; NodFactory derives them from the live profile.
/// Records a Nod in its group, opening the group with `call_terms` when it is the first.
/// `gratis_load_minor` joins the group's unpaid load.
pub fn add_nod(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    call_terms: Option<CallTerms>,
    gratis_load_minor: U256,
) -> Result<()> {
    crate::state_api::add_item(storage, item, call_terms, gratis_load_minor)
}

/// Removes a loaded Nod item and decrements membership, deleting the group only if empty.
pub fn remove_nod(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    group: LoadedNodGroup,
) -> Result<()> {
    crate::state_api::remove_item(storage, item, group)
}

/// Loads a Nod item while retaining its exact stored state for mutation.
pub fn load_item(
    storage: &StorageHandle<'_>,
    nod_id: WwdEntityId,
) -> Result<Option<LoadedNodItem>> {
    crate::state_api::load_item(storage, nod_id)
}

/// Loads a Nod group while retaining its exact stored state for mutation.
pub fn load_group(storage: &StorageHandle<'_>, group_key: B256) -> Result<Option<LoadedNodGroup>> {
    crate::state_api::load_group(storage, group_key)
}

/// Fetches a Nod item state by ID.
pub fn get_item(storage: &StorageHandle<'_>, nod_id: WwdEntityId) -> Result<Option<NodItemState>> {
    load_item(storage, nod_id).map(|loaded| loaded.map(|loaded| loaded.into_body()))
}

/// Fetches a Nod group state by ID.
pub fn get_group(storage: &StorageHandle<'_>, group_key: B256) -> Result<Option<NodGroupState>> {
    load_group(storage, group_key).map(|loaded| loaded.map(|loaded| loaded.into_body()))
}

/// The `index`-th Nod in the global index, read in one slot.
pub fn token_by_index(storage: &StorageHandle<'_>, index: u32) -> Result<Option<U256>> {
    Ok(NodContract::new(storage.clone())
        .all_ids
        .get(index)?
        .map(|id| id.to_u256()))
}

/// The owner's `index`-th Nod in its owner index, read in one slot.
pub fn token_of_owner_by_index(
    storage: &StorageHandle<'_>,
    owner: Address,
    index: u32,
) -> Result<Option<U256>> {
    Ok(NodContract::new(storage.clone())
        .owner_ids
        .list(&owner)
        .get(index)?
        .map(|id| id.to_u256()))
}

/// Loads the complete Nod collection.
pub fn list_all(storage: &StorageHandle<'_>) -> Result<Vec<NodItemState>> {
    NodContract::new(storage.clone())
        .state_records(None)
        .map(|records| records.into_iter().map(Into::into).collect())
}

/// How many Nods one owner holds, read from the owner index alone.
pub fn balance_of(storage: &StorageHandle<'_>, owner: Address) -> Result<u32> {
    NodContract::new(storage.clone())
        .owner_ids
        .list(&owner)
        .len()
}

/// Loads one owner's Nod collection.
pub fn list_by_owner(storage: &StorageHandle<'_>, owner: Address) -> Result<Vec<NodItemState>> {
    NodContract::new(storage.clone())
        .state_records(Some(owner))
        .map(|records| records.into_iter().map(Into::into).collect())
}

/// Records payment without consuming the entitlement or changing live supply.
pub fn record_settled(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    group: LoadedNodGroup,
) -> Result<()> {
    crate::state_api::record_settled(storage, item, group)
}

/// Internal arithmetic boundary; stored records and public views keep the amount encrypted.
pub fn calculation_amount(item: &NodItemState) -> Result<U256> {
    crate::enclave_client::read_amount(&item.encrypted)
        .map_err(|error| crate::errors::NodError::AmountReadFailed(error).into())
}
