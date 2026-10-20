//! Cross-module API for the Gem entity store.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::call_breach::group_key;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::first_full_day;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::schema::{
    CallTerms, GemAddParams, GemContract, GemGroupState, GemItemState, GemState, GemView,
};

pub use crate::state_api::{LoadedGem, LoadedGemGroup};

/// Records a gem in the group of its first full day, call price and reference currency,
/// opening that group if it is the first. Returns the gem id. GemFactory decides the issue.
pub fn add_gem(storage: &StorageHandle<'_>, params: GemAddParams) -> Result<U256> {
    let gem_id = GemContract::generate_gem_id(
        params.owner,
        params.promis_load_minor,
        storage.block_number()?,
    );
    let call_price = params
        .call_terms
        .map_or(U256::ZERO, |terms| terms.call_price_minor);
    let item = GemItemState {
        gem_id: WwdEntityId::from(gem_id),
        owner: params.owner,
        promis_load_minor: params.promis_load_minor,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        issued_at: params.issued_at,
        group_key: group_key(
            first_full_day(params.issued_at),
            call_price,
            params.reference_currency,
        ),
        state: GemState::Issued as u8,
        called_at: 0,
        settled_at: 0,
        entry_price_minor: params.entry_price_minor,
    };
    record_issued(storage, &item, params.call_terms)?;
    Ok(gem_id)
}

/// Records an already built gem, opening its group with `call_terms` if it is the first.
pub fn record_issued(
    storage: &StorageHandle<'_>,
    item: &GemItemState,
    call_terms: Option<CallTerms>,
) -> Result<()> {
    crate::state_api::add_item(storage, item, call_terms)
}

/// Loads a gem while retaining its exact stored state for mutation.
pub fn load_item(storage: &StorageHandle<'_>, gem_id: U256) -> Result<Option<LoadedGem>> {
    crate::state_api::load_item(storage, WwdEntityId::from(gem_id))
}

/// Loads a gem group while retaining its exact stored state for mutation.
pub fn load_group(storage: &StorageHandle<'_>, group_key: B256) -> Result<Option<LoadedGemGroup>> {
    crate::state_api::load_group(storage, group_key)
}

/// Loads a gem together with its group. A gem whose group record is missing is
/// corruption: the group outlives every member.
pub fn load_pair(
    storage: &StorageHandle<'_>,
    gem_id: U256,
) -> Result<Option<(LoadedGem, LoadedGemGroup)>> {
    let Some(item) = load_item(storage, gem_id)? else {
        return Ok(None);
    };
    let group = load_group(storage, item.body().group_key)?.ok_or_else(|| {
        crate::errors::GemFatalError::MissingGroupRecord {
            gem_id,
            group_id: item.body().group_id(),
        }
    })?;
    Ok(Some((item, group)))
}

/// Fetches a gem by id.
pub fn get_item(storage: &StorageHandle<'_>, gem_id: U256) -> Result<Option<GemItemState>> {
    load_item(storage, gem_id).map(|loaded| loaded.map(|loaded| loaded.into_body()))
}

/// Fetches a gem group by id.
pub fn get_group(storage: &StorageHandle<'_>, group_key: B256) -> Result<Option<GemGroupState>> {
    load_group(storage, group_key).map(|loaded| loaded.map(|loaded| loaded.into_body()))
}

/// Fetches the full status view of a gem: gem, group and sealed terms.
pub fn get_view(storage: &StorageHandle<'_>, gem_id: U256) -> Result<Option<GemView>> {
    let Some((item, group)) = load_pair(storage, gem_id)? else {
        return Ok(None);
    };
    Ok(Some(view_of(storage, item.into_body(), group.into_body())?))
}

/// Assembles the view from an already loaded gem and group.
pub fn view_of(
    storage: &StorageHandle<'_>,
    item: GemItemState,
    group: GemGroupState,
) -> Result<GemView> {
    let gem = GemContract::new(storage.clone());
    let terms = gem.read_call_terms(item.group_key)?;
    let group_called_at = gem.group_called_at.read(&item.group_key)?;
    Ok(GemView {
        item,
        group,
        terms,
        group_called_at,
    })
}

/// How many gems one owner holds, read from the owner index alone.
pub fn balance_of(storage: &StorageHandle<'_>, owner: Address) -> Result<u32> {
    GemContract::new(storage.clone())
        .owner_ids
        .list(&owner)
        .len()
}

/// The owner's `index`-th gem in its owner index, read in one slot.
pub fn token_of_owner_by_index(
    storage: &StorageHandle<'_>,
    owner: Address,
    index: u32,
) -> Result<Option<U256>> {
    Ok(GemContract::new(storage.clone())
        .owner_ids
        .list(&owner)
        .get(index)?
        .map(|id| id.to_u256()))
}

/// One owner's gems, read through the owner index.
pub fn list_by_owner(storage: &StorageHandle<'_>, owner: Address) -> Result<Vec<GemItemState>> {
    GemContract::new(storage.clone()).state_records(owner)
}

/// Timestamp by which a called group must be settled, or `0` while it is not called.
pub fn settlement_deadline(storage: &StorageHandle<'_>, group_key: B256) -> Result<u64> {
    let gem = GemContract::new(storage.clone());
    let called_at = gem.group_called_at.read(&group_key)?;
    if called_at == 0 {
        return Ok(0);
    }
    let notice = gem.group_call_notice_period_seconds.read(&group_key)?;
    Ok(settlement_deadline_of(called_at, notice))
}

pub use outbe_primitives::settlement::settlement_deadline_of;

/// Records a loaded gem paid for. GemFactory decides the settlement and takes the payment.
pub fn record_settled(
    storage: &StorageHandle<'_>,
    item: LoadedGem,
    group: LoadedGemGroup,
) -> Result<()> {
    crate::state_api::record_settled(storage, item, group)
}

/// Deletes a loaded gem, lowering membership or the paid count, and the group once
/// nothing refers to it.
pub fn remove_gem(
    storage: &StorageHandle<'_>,
    item: LoadedGem,
    group: LoadedGemGroup,
) -> Result<()> {
    crate::state_api::remove_item(storage, item, group)
}
