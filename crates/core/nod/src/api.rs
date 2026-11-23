//! Cross-module API for the Nod entity store.

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::series_id::SeriesId;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::schema::{CallTerms, EffectiveState, NodContract, NodGroupState, NodItemState};

/// Derives the current entitlement state without waiting for sweep cleanup.
/// Settled entitlements survive expiry; the settlement deadline is inclusive.
#[must_use]
pub fn effective_state(
    item: &NodItemState,
    called_at: u64,
    deadline: u64,
    now: U256,
) -> EffectiveState {
    use outbe_core_common::settlement::RightState;
    match outbe_core_common::settlement::effective_state(
        item.is_settled,
        called_at,
        deadline,
        now.saturating_to::<u64>(),
    ) {
        RightState::Issued => EffectiveState::Issued,
        RightState::Called => EffectiveState::Called,
        RightState::Settled => EffectiveState::Settled,
        RightState::Forfeited => EffectiveState::Forfeited,
    }
}

/// The Nod's settlement cost: `floor(entry_price_minor * gratis_load_minor / 1e6)`.
/// Positive inputs have a minimum cost of one reference-currency minor unit.
/// Price and cost use six-decimal reference-currency precision. The load uses
/// protocol units (1e6 per whole COEN). Asset payment units are quoted separately.
///
/// The cost is derived rather than stored from the entry price and the load the
/// Nod carries. Lysis mints the Nod from exactly this formula.
pub use outbe_core_common::nod_rules::settlement_cost_minor;

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

pub use outbe_core_common::settlement::settlement_deadline_of;

/// The Nod's state as of this block, as `getNod` reports it: an unsettled Nod of a
/// forfeited group reads Forfeited.
pub fn state_of(storage: &StorageHandle<'_>, item: &NodItemState) -> Result<EffectiveState> {
    let nod = NodContract::new(storage.clone());
    if !item.is_settled && nod.group_is_forfeited.read(&item.group_key)? {
        return Ok(EffectiveState::Forfeited);
    }
    let called_at = nod.group_called_at.read(&item.group_key)?;
    let deadline = settlement_deadline(storage, item.group_key)?;
    Ok(effective_state(
        item,
        called_at,
        deadline,
        storage.timestamp()?,
    ))
}

pub use crate::state_api::{LoadedNodGroup, LoadedNodItem};

/// Inserts a Nod item, increments membership and creates its group if absent atomically.
/// A new group seals `call_terms`; NodFactory derives them from the live profile.
/// Records a Nod in its group, opening the group with `call_terms` when it is the first.
/// `gratis_load_minor` joins the group's unsettled load.
pub fn add_nod(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    call_terms: Option<CallTerms>,
) -> Result<()> {
    crate::state_api::add_item(storage, item, call_terms)
}

/// Decrypts one offer, prices it and encrypts its Nod in the enclave.
pub fn issue_at_offer(
    storage: &StorageHandle<'_>,
    request: crate::offer::issue::IssueNodAtOfferRequestV1,
) -> Result<std::result::Result<crate::offer::issue::NodAtOfferResultV1, outbe_tee::TransportError>>
{
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    Ok(crate::enclave_client::issue_nod_at_offer(request))
}

/// Adds the load, or with `nominal` the nominal, of `nod` to the day sum `previous`
/// of `scope`.
pub fn add_to_day_sum(
    storage: &StorageHandle<'_>,
    scope: B256,
    previous: Option<outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1>,
    (nod, nominal): (&outbe_core_common::nod_encryption::EncryptedNodV1, bool),
) -> Result<outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1> {
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let nod = Box::new(nod.clone());
    let operation = if nominal {
        crate::nod_encryption::NodGroupLoadOperationV1::AddNodNominal { nod }
    } else {
        crate::nod_encryption::NodGroupLoadOperationV1::AddNod { nod }
    };
    let request = crate::nod_encryption::NodGroupLoadOpRequestV1 {
        chain_id: storage.chain_id()?,
        scope,
        previous,
        operation,
    };
    crate::enclave_client::apply_group_load_operation(request)
        .map_err(|err| crate::errors::NodError::AmountReadFailed(err).into())
}

/// Reads a day sum in the enclave.
pub fn read_day_sum(
    storage: &StorageHandle<'_>,
    record: &outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1,
) -> Result<U256> {
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    crate::enclave_client::read_group_load(record)
        .map_err(|err| crate::errors::NodError::AmountReadFailed(err).into())
}

/// The call group sub-series `number` of `series_id` joined with its first Nod.
pub fn sub_series_group(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    number: u16,
) -> Result<Option<B256>> {
    let key = crate::index_keys::sub_series_key(series_id, number);
    let group_key = NodContract::new(storage.clone())
        .sub_series_group
        .read(&key)?;
    Ok((!group_key.is_zero()).then_some(group_key))
}

/// The entry price sub-series `number` of `series_id` got with its first Nod.
pub fn sub_series_entry_price(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    number: u16,
) -> Result<Option<U256>> {
    let key = crate::index_keys::sub_series_key(series_id, number);
    let price = NodContract::new(storage.clone())
        .sub_series_entry_price_minor
        .read(&key)?;
    Ok((!price.is_zero()).then_some(price))
}

/// Returns whether `group_key` has been forfeited after its call notice period lapsed.
pub fn is_group_forfeited(storage: &StorageHandle<'_>, group_key: B256) -> Result<bool> {
    NodContract::new(storage.clone())
        .group_is_forfeited
        .read(&group_key)
}

/// Whether the forfeit pass has started on `group_key` or finished it.
pub fn is_group_forfeit_started(storage: &StorageHandle<'_>, group_key: B256) -> Result<bool> {
    let nod = NodContract::new(storage.clone());
    Ok(nod.group_forfeiting.read(&group_key)? || nod.group_is_forfeited.read(&group_key)?)
}

/// Permissionless cleanup of an unsettled forfeited Nod (`INod.clean(uint256)`).
pub fn clean(storage: &StorageHandle<'_>, nod_id: U256) -> Result<()> {
    crate::state_api::clean_item(storage, nod_id)
}

/// Removes `load` from the group's unsettled total.
///
/// `remove_nod` leaves an unsettled Nod's load on the old group. A storage-only
/// reissue calls this first, then removes the item and adds it to the new group.
pub fn take_unsettled_load(storage: &StorageHandle<'_>, group_key: B256, load: U256) -> Result<()> {
    NodContract::new(storage.clone()).take_unsettled_load(group_key, load)
}

/// Removes `load` from both `group_key` and `(group_key, sub_series)` open loads.
pub fn take_series_unsettled_load(
    storage: &StorageHandle<'_>,
    group_key: B256,
    sub_series: B256,
    load: U256,
) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    nod.take_unsettled_load(group_key, load)?;
    nod.take_series_unsettled_load(group_key, sub_series, load)
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

/// Seals a settlement quote to the Nod owner's view key via the enclave.
pub fn encrypt_quote_for_owner(
    nod: &outbe_core_common::nod_encryption::EncryptedNodV1,
    context: outbe_core_common::nod_encryption::NodQuoteContextV1,
    payment_minor: U256,
) -> Result<Vec<u8>> {
    crate::enclave_client::encrypt_nod_quote(nod, context, payment_minor)
        .map_err(|error| crate::errors::NodError::AmountReadFailed(error).into())
}

/// Decrypts an encrypted group open load via the enclave (used on group forfeiture or test inspection).
pub fn read_group_open_load(storage: &StorageHandle<'_>, group_key: B256) -> Result<U256> {
    NodContract::new(storage.clone()).read_group_open_load(group_key)
}
