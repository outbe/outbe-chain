//! Cross-module API for the Nod entity store.

use alloy_primitives::{Address, B256, U256};
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

/// Inserts a Nod item claimed from a Lysis series, moving `gratis_load_minor` from
/// `group_unclaimed_tribute_load` to `group_unpaid_load` and decrementing `group_unclaimed_tribute_count`.
pub fn add_series_nod(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    call_terms: Option<CallTerms>,
    gratis_load_minor: U256,
    series_group_key: B256,
) -> Result<()> {
    crate::state_api::add_series_item(
        storage,
        item,
        call_terms,
        gratis_load_minor,
        series_group_key,
    )
}

/// Parameters for sealing a Lysis series group `(E, issuance_currency, reference_currency)`.
#[derive(Debug, Clone, Copy)]
pub struct LysisSeriesSeal {
    pub group_key: B256,
    pub full_day_after_issue: outbe_primitives::time::WorldwideDay,
    pub reference_currency: u16,
    pub entry_price_minor: U256,
    pub call_terms: Option<CallTerms>,
    pub unclaimed_tribute_count: u64,
    pub unclaimed_tribute_load: U256,
}

/// Seals a Lysis series group `(E, issuance_currency, reference_currency)` with its
/// initial `unclaimed_tribute_count` and exact `unclaimed_tribute_load`.
pub fn seal_lysis_series(storage: &StorageHandle<'_>, seal: LysisSeriesSeal) -> Result<()> {
    storage.with_checkpoint(|| NodContract::new(storage.clone()).seal_lysis_series_state(seal))
}

/// Returns whether `group_key` has been forfeited after its call notice period lapsed.
pub fn is_group_forfeited(storage: &StorageHandle<'_>, group_key: B256) -> Result<bool> {
    NodContract::new(storage.clone())
        .group_is_forfeited
        .read(&group_key)
}

/// Reads the entry price sealed on `group_key` at Lysis, or `U256::ZERO` if none.
pub fn group_entry_price_minor(storage: &StorageHandle<'_>, group_key: B256) -> Result<U256> {
    NodContract::new(storage.clone())
        .group_entry_price_minor
        .read(&group_key)
}

/// Decrements `group_unclaimed_tribute_count` when a forfeited Tribute is cleaned,
/// deleting the group if all counters (`group_nod_count`, `settled_nods`, `unclaimed_tribute_count`) reach zero.
pub fn record_unclaimed_tribute_cleaned(
    storage: &StorageHandle<'_>,
    group_key: B256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        NodContract::new(storage.clone()).record_unclaimed_tribute_cleaned_state(group_key)
    })
}

/// Permissionless cleanup of an unpaid forfeited Nod (`INod.clean(uint256)`).
pub fn clean(storage: &StorageHandle<'_>, nod_id: U256) -> Result<()> {
    crate::state_api::clean_item(storage, nod_id)
}

/// Removes `load` from the group's unpaid total.
///
/// `remove_nod` leaves an unsettled Nod's load on the old group. A storage-only
/// reissue calls this first, then removes the item and adds it to the new group.
pub fn take_unpaid_load(storage: &StorageHandle<'_>, group_key: B256, load: U256) -> Result<()> {
    NodContract::new(storage.clone()).take_unpaid_load(group_key, load)
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
pub fn load_item(storage: &StorageHandle<'_>, nod_id: U256) -> Result<Option<LoadedNodItem>> {
    crate::state_api::load_item(storage, nod_id)
}

/// Loads a Nod group while retaining its exact stored state for mutation.
pub fn load_group(storage: &StorageHandle<'_>, group_key: B256) -> Result<Option<LoadedNodGroup>> {
    crate::state_api::load_group(storage, group_key)
}

/// Fetches a Nod item state by ID.
pub fn get_item(storage: &StorageHandle<'_>, nod_id: U256) -> Result<Option<NodItemState>> {
    load_item(storage, nod_id).map(|loaded| loaded.map(|loaded| loaded.into_body()))
}

/// Fetches a Nod group state by ID.
pub fn get_group(storage: &StorageHandle<'_>, group_key: B256) -> Result<Option<NodGroupState>> {
    load_group(storage, group_key).map(|loaded| loaded.map(|loaded| loaded.into_body()))
}

/// The ids of one owner's Nods, read from the owner index alone.
pub fn tokens_by_owner(storage: &StorageHandle<'_>, owner: Address) -> Result<Vec<U256>> {
    NodContract::new(storage.clone())
        .owner_ids
        .list(&owner)
        .read_all()
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
        .state_records(owner)
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
