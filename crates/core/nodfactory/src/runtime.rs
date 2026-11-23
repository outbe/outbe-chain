//! NodFactory runtime: issuance, settlement, PoW-gated mining, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_system_common::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::NOD_FACTORY_ADDRESS;

use outbe_core_common::nod_encryption::EncryptedNodV1;
use outbe_core_common::pow;
use outbe_nod::api as nod_api;
use outbe_nod::api::{LoadedNodGroup, LoadedNodItem};
use outbe_nod::schema::{CallTerms, NodItemState};

use crate::errors::NodFactoryError;
use crate::precompile::INodFactory;
use outbe_vaultrouter::settlement::{self, Currencies, SettlementCost, SettlementPayment};

/// Issues an authenticated encrypted Nod through the block-scoped body lifecycle.
pub fn issue_nod(storage: &StorageHandle<'_>, encrypted: &EncryptedNodV1) -> Result<U256> {
    issue_nod_at(storage, encrypted, storage.now_secs()?)
}

pub(crate) fn issue_nod_at(
    storage: &StorageHandle<'_>,
    encrypted: &EncryptedNodV1,
    issued_at: u64,
) -> Result<U256> {
    issue_nod_with_cost(storage, encrypted, (issued_at, 0))
}

/// Issues the Nod of an offer into sub-series `sub_series_id` of its series. The first Nod
/// of a sub-series opens it with the entry price and the call terms of its group.
pub(crate) fn issue_offer_nod(
    storage: &StorageHandle<'_>,
    encrypted: &EncryptedNodV1,
    issued_at: u64,
    sub_series_id: u16,
) -> Result<U256> {
    let series_id = encrypted
        .terms
        .series_id()
        .map_err(|_| NodFactoryError::InvalidEncryptedNod)?;
    let opens = nod_api::sub_series_entry_price(storage, series_id, sub_series_id)?.is_none();
    let nod_id = issue_nod_with_cost(storage, encrypted, (issued_at, sub_series_id))?;
    if opens {
        let group_key = nod_api::get_item(storage, nod_id)?
            .ok_or(NodFactoryError::NodNotFound)?
            .group_key;
        let terms =
            outbe_nod::schema::NodContract::new(storage.clone()).read_call_terms(group_key)?;
        emit_event(
            storage,
            INodFactory::NodSeriesOpened {
                seriesId: series_id.into(),
                issuanceCurrency: encrypted.terms.issuance_currency,
                referenceCurrency: encrypted.terms.reference_currency,
                entryPriceMinor: encrypted.terms.entry_price_minor,
                callPriceMinor: terms.call_price_minor,
                callWindow: u64::from(terms.call_window_seconds),
                callThreshold: u64::from(terms.call_threshold_seconds),
                callNoticePeriod: u64::from(terms.call_notice_period_seconds),
            },
        )?;
    }
    Ok(nod_id)
}

fn issue_nod_with_cost(
    storage: &StorageHandle<'_>,
    encrypted: &EncryptedNodV1,
    (issued_at, sub_series_id): (u64, u16),
) -> Result<U256> {
    let terms = &encrypted.terms;
    if terms.owner.is_zero() {
        return Err(NodFactoryError::InvalidOwner.into());
    }
    let series_id = terms
        .series_id()
        .map_err(|_| NodFactoryError::InvalidEncryptedNod)?;
    let nod_id = outbe_nod::identity::generate_nod_id(series_id, terms.owner);
    if terms.nod_id != nod_id
        || terms.chain_id != storage.chain_id()?
        || !encrypted.has_valid_encoding()
    {
        return Err(NodFactoryError::InvalidEncryptedNod.into());
    }
    if nod_api::get_item(storage, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }
    if !outbe_nod::pricing::is_issuable_entry(terms.entry_price_minor) {
        return Err(NodFactoryError::EntryPriceOutOfBounds.into());
    }
    let call_terms = call_terms(storage, terms.entry_price_minor, terms.reference_currency)?;
    let call_price_minor = call_terms.map_or(U256::ZERO, |t| t.call_price_minor);
    let group_key = match nod_api::sub_series_group(storage, series_id, sub_series_id)? {
        Some(group_key) => group_key,
        None => outbe_nod::identity::group_key(
            terms.emission_day,
            call_price_minor,
            terms.reference_currency,
        ),
    };
    let item = NodItemState {
        is_settled: false,
        nod_id,
        owner: terms.owner,
        encrypted: encrypted.clone(),
        series_id,
        sub_series_id,
        group_key,
        issuance_currency: terms.issuance_currency,
        reference_currency: terms.reference_currency,
        issued_at,
    };
    let gratis_load_minor = nod_api::calculation_amount(&item)?;
    let _ = nod_api::settlement_cost_minor(terms.entry_price_minor, gratis_load_minor)?;
    nod_api::add_nod(storage, &item, call_terms)?;
    emit_event(
        storage,
        INodFactory::NodIssued {
            nodId: nod_id,
            owner: terms.owner,
            seriesId: series_id.into(),
            issuanceCurrency: terms.issuance_currency,
            referenceCurrency: terms.reference_currency,
            payload: encrypted.encrypted_gratis_amount.clone().into(),
        },
    )?;
    Ok(nod_id)
}

/// Mining of one settled Nod. Any caller may submit. The owner's Gratis
/// modify-key MAC/`opNonce` authorizes the mint to that owner.
pub struct MineGratisRequest {
    pub caller: Address,
    pub nod_id: U256,
    pub nonce: u64,
    pub auth: outbe_gratisfactory::api::ModifyAuth,
}

/// Pays an uncalled or called Nod's known cost in ERC20 base units of `asset`. An
/// issuance-currency payment must name the VWAP snapshot required at this block, and
/// the cost must fit the payer's cap.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    nod_id: U256,
    payment: SettlementPayment,
    amount_cap_minor: U256,
) -> Result<()> {
    let SettlementPayment {
        payer,
        asset,
        snapshot_id,
    } = payment;
    let (item, group) = load_nod(storage, nod_id)?;
    if item.body().is_settled {
        return Err(NodFactoryError::NodAlreadySettled.into());
    }
    let deadline = nod_api::settlement_deadline(storage, item.body().group_key)?;
    if deadline != 0 && storage.now_secs()? > deadline {
        return Err(NodFactoryError::SettlementDeadlinePassed.into());
    }
    let owner = item.body().owner;
    let issuance_currency = item.body().issuance_currency;
    let reference_currency = item.body().reference_currency;
    let quote = settlement::quote(
        storage,
        asset,
        currencies(item.body()),
        &settlement_cost(item.body())?,
    )?;
    quote.require_snapshot(snapshot_id)?;
    settlement::require_funds(storage, payer, asset, amount_cap_minor)?;
    quote.require_within_cap(amount_cap_minor)?;
    storage.clone().with_checkpoint(|| {
        // Publish the transition before external payment calls so callbacks cannot
        // settle the same Nod twice. A failed payment rolls the transition back.
        nod_api::record_settled(storage, item, group)?;
        settlement::collect(storage, payer, asset, quote.payment_minor)?;
        emit_event(
            storage,
            INodFactory::NodSettled {
                nodId: nod_id,
                owner,
                asset,
                issuanceCurrency: issuance_currency,
                referenceCurrency: reference_currency,
                amountMinor: quote.payment_minor,
            },
        )
    })
}

/// Cost of one Nod at `entry_price_minor` and `gratis_load_minor` in an asset.
#[cfg(test)]
pub(crate) fn settlement_units(
    entry_price_minor: U256,
    gratis_load_minor: U256,
    rate: Option<(U256, U256)>,
    asset_decimals: u8,
) -> Result<U256> {
    let cost = nod_cost(entry_price_minor, gratis_load_minor);
    outbe_core_common::settlement::settlement_cost_units(&cost, rate, asset_decimals)
        .map_err(|e| settlement::SettlementError::Rounding(e).into())
}

fn currencies(item: &NodItemState) -> Currencies {
    Currencies {
        issuance: item.issuance_currency,
        reference: item.reference_currency,
    }
}

/// One Nod: its entry price times its Gratis load.
fn settlement_cost(item: &NodItemState) -> Result<SettlementCost> {
    Ok(nod_cost(
        item.entry_price_minor(),
        nod_api::calculation_amount(item)?,
    ))
}

/// One Nod at its entry price and Gratis load, at cost.
fn nod_cost(entry_price_minor: U256, gratis_load_minor: U256) -> SettlementCost {
    SettlementCost {
        entry_price_minor,
        load_minor: gratis_load_minor,
        units: U256::ONE,
    }
}

/// The call terms a group opened at `entry_price_minor` seals: the entry marked up by
/// the live profile's call rate, plus the profile's window, threshold and notice.
///
/// `None` when entry is zero: a zero call price would fire on the first scan. Every
/// later check reads the group's sealed copy, so a retune cannot re-term an issued Nod.
pub(crate) fn call_terms(
    storage: &StorageHandle<'_>,
    entry_price_minor: U256,
    reference_currency: u16,
) -> Result<Option<CallTerms>> {
    if entry_price_minor.is_zero() {
        return Ok(None);
    }
    let params = outbe_nod::config::read(storage)?;
    let call_price = outbe_core_common::markup::marked_up(entry_price_minor, params.call_rate)
        .ok_or(crate::errors::NodFactoryError::CallPriceOverflow)?;
    Ok(Some(CallTerms {
        call_price_minor: call_price,
        reference_currency,
        call_rate: params.call_rate,
        call_window_seconds: params.call_window_seconds,
        call_threshold_seconds: params.call_threshold_seconds,
        call_notice_period_seconds: params.call_notice_period_seconds,
    }))
}

/// Mines a settled entitlement without payment or a deadline. Mint failure
/// rolls back the removal, preserving the settled Nod for retry.
pub fn mine_gratis(storage: &StorageHandle<'_>, request: MineGratisRequest) -> Result<Bytes> {
    let MineGratisRequest {
        nod_id,
        nonce,
        auth,
        ..
    } = request;
    let (item, group) = load_nod(storage, nod_id)?;
    if !item.body().is_settled {
        return Err(NodFactoryError::NodNotSettled.into());
    }
    let owner = item.body().owner;
    validate_pow(nod_id, owner, nonce)?;
    let encrypted = item.body().encrypted.clone();
    storage.clone().with_checkpoint(|| {
        nod_api::remove_nod(storage, item, group)?;
        // The Nod owner authorizes the mint, including when a relayer submits it.
        outbe_gratisfactory::api::mint_encrypted_nod(storage.clone(), &encrypted, auth)?;
        outbe_gratisfactory::api::encrypted_balance(storage.clone(), owner)
    })
}

fn load_nod(storage: &StorageHandle<'_>, nod_id: U256) -> Result<(LoadedNodItem, LoadedNodGroup)> {
    let item = nod_api::load_item(storage, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    let group = nod_api::load_group(storage, item.body().group_key)?
        .ok_or(NodFactoryError::GroupNotFound)?;
    Ok((item, group))
}

/// What settling `nod_id` with `asset` costs (sealed to the Nod owner's view key),
/// in which currency, and the VWAP snapshot an issuance-currency payment must name
/// (zero on the reference rail). Reverts with `NotNodOwner` when `caller != owner`.
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    caller: Address,
    nod_id: U256,
    asset: Address,
) -> Result<(u16, Bytes, U256)> {
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let (item, _) = load_nod(storage, nod_id)?;
    if item.body().owner != caller {
        return Err(NodFactoryError::NotNodOwner.into());
    }
    let quote = settlement::quote(
        storage,
        asset,
        currencies(item.body()),
        &settlement_cost(item.body())?,
    )?;
    let snapshot_id = quote.snapshot_id();
    let encrypted = nod_api::encrypt_quote_for_owner(
        &item.body().encrypted,
        outbe_core_common::nod_encryption::NodQuoteContextV1 {
            chain_id: storage.chain_id()?,
            nod_id,
            owner: item.body().owner,
            asset,
            settlement_currency: quote.settlement_currency,
            snapshot_id,
        },
        quote.payment_minor,
    )?;
    Ok((
        quote.settlement_currency,
        Bytes::from(encrypted),
        snapshot_id,
    ))
}

/// PoW gate for `mine_gratis`. The preimage is
/// `OUTBE_NOD_MINING_V1 || nodId || owner || miningSequence=0 || nonce`. The caller is not in
/// it.
pub fn validate_pow(nod_id: U256, owner: Address, nonce: u64) -> Result<()> {
    pow::validate_mining_pow(
        pow::MiningDomain::Nod,
        nod_id,
        owner,
        pow::SINGLE_MINING_SEQUENCE,
        nonce,
    )
    .map_err(|e| NodFactoryError::from(e).into())
}

/// SHA256 over `OUTBE_NOD_MINING_V1 || nodId_be32 || owner_20 || miningSequence_be8 ||
/// nonce_be8` with `miningSequence = 0`.
pub fn compute_pow_hash(nod_id: U256, owner: Address, nonce: u64) -> [u8; 32] {
    pow::compute_mining_pow_hash(
        pow::MiningDomain::Nod,
        nod_id,
        owner,
        pow::SINGLE_MINING_SEQUENCE,
        nonce,
    )
}

pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
