use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gem::{api as gem_api, CallTerms, GemAddParams, GemParams, GemView};
use outbe_intex::{IntexState, SeriesId};
use outbe_oracle::api::get_utc_day_vwap_for_iso;
use outbe_primitives::addresses::{GEM_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS};
use outbe_primitives::error::Result;
use outbe_primitives::markup::marked_up;
use outbe_primitives::stablecoin::iso_4217_alpha;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use outbe_common::pow;

use crate::errors::{date_step_error, GemFactoryError};
use crate::precompile::IGemFactory::{GemIssued, GemMined, GemPositionIssued, GemSettled};
use crate::schema::{GemFactoryContract, GemIssueParams, GemPosition};
use crate::sol_ext::IIntexNFT1155;
use outbe_vaultrouter::settlement::{self, Currencies, SettlementCost, SettlementPayment};

fn maintenance_context<'s>(
    storage: &StorageHandle<'s>,
) -> Result<outbe_primitives::block::BlockRuntimeContext<'s>> {
    use outbe_primitives::block::{BlockContext, BlockContextInput, BlockRuntimeContext};
    Ok(BlockRuntimeContext::new(
        BlockContext::new_with_genesis_hash(BlockContextInput {
            block_number: storage.block_number()?,
            timestamp: storage.now_secs()?,
            chain_id: storage.chain_id()?,
            genesis_hash: storage.genesis_hash()?,
            proposer: Address::ZERO,
            validators: Vec::new(),
        }),
        storage.clone(),
    ))
}

pub fn maintain(
    storage: &StorageHandle<'_>,
    selection: outbe_oracle::call_sweep::Selection,
) -> Result<()> {
    outbe_primitives::dispatch::ensure_mutation_allowed(storage)?;
    // TODO: Reward callers for maintenance work performed.
    if !selection.process_call && !selection.process_forfeit && !selection.process_cleanup {
        return Ok(());
    }
    let ctx = maintenance_context(storage)?;
    crate::called::maintain(&ctx, selection)?;
    if selection.process_cleanup {
        crate::cleanup::sweep_cleanup(&ctx)?;
        crate::positions_expired::sweep_position_cleanup(&ctx)?;
    }
    Ok(())
}

pub fn has_maintenance_work(storage: &StorageHandle<'_>) -> Result<(bool, bool, bool)> {
    let ctx = maintenance_context(storage)?;
    let (call, forfeit, _) = crate::called::pending_work(&ctx)?;
    let records = crate::cleanup::pending_work(&ctx)?;
    let positions = crate::positions_expired::cleanup_pending(&ctx)?;
    Ok((call, forfeit, records || positions))
}

/// Issues one reward gem priced at `entry_price`, with its call price on
/// `call_anchor_price_minor`. The caller resolves both for the gem's own day.
pub fn issue_gem(storage: &StorageHandle<'_>, params: GemIssueParams) -> Result<U256> {
    let GemIssueParams {
        owner,
        promis_load_minor: promis_load,
        issuance_currency,
        reference_currency,
        entry_price_minor: entry_price,
        call_anchor_price_minor: call_anchor,
    } = params;
    if owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }
    if entry_price.is_zero() || call_anchor.is_zero() {
        return Err(GemFactoryError::OracleUnavailable.into());
    }
    if promis_load.is_zero() {
        return Err(GemFactoryError::ZeroPromisLoad.into());
    }
    if iso_4217_alpha(issuance_currency).is_none() {
        return Err(GemFactoryError::InvalidCurrency {
            currency: issuance_currency,
        }
        .into());
    }
    outbe_oracle::api::check_reference_currency_with_storage(storage.clone(), reference_currency)?;

    // The caller resolves the price: it knows which day the gem belongs to.
    let issued_at = storage.now_secs()?;
    let params = GemAddParams {
        owner,
        promis_load_minor: promis_load,
        entry_price_minor: entry_price,
        issuance_currency,
        reference_currency,
        issued_at,
        call_terms: None,
    };
    let gem_id = add_gem_with_call_anchor(storage, params, call_anchor)?;

    let factory = GemFactoryContract::new(storage.clone());
    let prev_total = factory.total_gems_issued.read()?;
    let new_total = prev_total
        .checked_add(U256::from(1))
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gems_issued.write(new_total)?;

    emit_gem_issued(storage, gem_id, U256::ZERO)?;

    Ok(gem_id)
}

/// Issues a gem whose call price marks up its own entry.
pub(crate) fn add_gem(storage: &StorageHandle<'_>, params: GemAddParams) -> Result<U256> {
    let call_anchor = params.entry_price_minor;
    add_gem_with_call_anchor(storage, params, call_anchor)
}

/// Issues a gem on the live profile: the group it opens seals the profile's call terms.
fn add_gem_with_call_anchor(
    storage: &StorageHandle<'_>,
    mut params: GemAddParams,
    call_anchor_price_minor: U256,
) -> Result<U256> {
    if params.owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }
    params.call_terms = derived_call_terms(
        call_anchor_price_minor,
        params.reference_currency,
        outbe_gem::config::read(storage)?,
    )?;
    gem_api::add_gem(storage, params)
}

/// The call anchor marked up by the call rate, plus the profile's other call terms.
///
/// `None` when the anchor is zero: a zero call price would fire on the first scan. The
/// profile is read exactly here. Every later check reads the group's sealed copy, so
/// a retune cannot re-term an already-issued gem.
pub(crate) fn derived_call_terms(
    call_anchor_price_minor: U256,
    reference_currency: u16,
    params: GemParams,
) -> Result<Option<CallTerms>> {
    if call_anchor_price_minor.is_zero() {
        return Ok(None);
    }
    let call_price = marked_up(call_anchor_price_minor, params.call_rate)
        .ok_or(GemFactoryError::CallPriceOverflow)?;
    Ok(Some(CallTerms {
        call_price_minor: call_price,
        reference_currency,
        call_rate: params.call_rate,
        call_window_seconds: params.call_window_seconds,
        call_threshold_seconds: params.call_threshold_seconds,
        call_notice_period_seconds: params.call_notice_period_seconds,
    }))
}

/// Announce a new gem with the terms its group sealed. A gem joining an open group
/// reports that group's terms, not the live profile.
fn emit_gem_issued(storage: &StorageHandle<'_>, gem_id: U256, position_id: U256) -> Result<()> {
    let gem = load_view(storage, gem_id)?;
    emit_event(
        storage,
        GemIssued {
            gemId: gem_id,
            owner: gem.item.owner,
            promisLoadMinor: gem.item.promis_load_minor,
            entryPriceMinor: gem.item.entry_price_minor,
            issuanceCurrency: gem.item.issuance_currency,
            referenceCurrency: gem.item.reference_currency,
            issuedAt: gem.item.issued_at,
            callPriceMinor: gem.terms.call_price_minor,
            callWindow: gem.terms.call_window_seconds,
            callThreshold: gem.terms.call_threshold_seconds,
            callNoticePeriod: gem.terms.call_notice_period_seconds,
            positionId: position_id,
            groupKey: gem.item.group_key,
        },
    )
}

fn load_view(storage: &StorageHandle<'_>, gem_id: U256) -> Result<GemView> {
    gem_api::get_view(storage, gem_id)?.ok_or_else(|| GemFactoryError::GemNotFound.into())
}

/// Issue a GemPosition NFT from the merchant's selected `units` of Issued Intex.
/// The merchant can transfer a partial holding. The position pins the source entry.
/// Its Promis capacity equals the source load times the burned units.
/// Return the issued `position_id`.
pub fn issue_gem_position(
    storage: &StorageHandle<'_>,
    caller: Address,
    source_intex_id: SeriesId,
    units: U256,
) -> Result<U256> {
    if caller.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }

    let series = outbe_intex::api::get_series(storage, source_intex_id)?
        .ok_or(GemFactoryError::SourceIntexNotFound)?;
    if outbe_intex::api::state(storage, &series)? != IntexState::Issued {
        return Err(GemFactoryError::SourceIntexNotIssued.into());
    }

    // The daily call scan walks only listed reference currencies.
    outbe_oracle::api::check_reference_currency_with_storage(
        storage.clone(),
        series.reference_currency,
    )?;

    let issued_at = storage.now_secs()?;

    // Burn `units` of the merchant's Intex. `sendToGemFactory` returns the
    // burned count (and reverts on a state that may not be sent, or zero units).
    let burned = burn_intex_into_gem_factory(storage, caller, source_intex_id, units)?;
    let capacity = series
        .promis_load_minor
        .checked_mul(burned)
        .ok_or(GemFactoryError::Overflow)?;

    // Their load moved into the position, so the source series cannot forfeit them.
    let gem_factory_units = u32::try_from(burned).map_err(|_| GemFactoryError::Overflow)?;
    outbe_intex::api::record_gem_factory_units(
        storage,
        source_intex_id,
        caller,
        gem_factory_units,
    )?;
    outbe_intex::api::release_if_spent(storage, source_intex_id)?;

    let position_id =
        GemFactoryContract::generate_position_id(caller, source_intex_id, storage.block_number()?);

    let mut factory = GemFactoryContract::new(storage.clone());
    let position = GemPosition {
        position_id,
        merchant: caller,
        source_intex_id,
        remaining_capacity_minor: capacity,
        source_entry_price_minor: series.entry_price_minor,
        issuance_currency: series.issuance_currency,
        reference_currency: series.reference_currency,
        issued_at,
        expires_at: issued_at.saturating_add(outbe_gem::config::read(storage)?.position_validity),
    };
    factory.add_position(&position)?;

    let prev_sent = factory.total_capacity_minor.read()?;
    let new_sent = prev_sent
        .checked_add(capacity)
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_capacity_minor.write(new_sent)?;

    emit_event(
        storage,
        GemPositionIssued {
            positionId: position_id,
            merchant: caller,
            sourceIntexId: source_intex_id.into(),
            capacityMinor: capacity,
            sourceEntryPriceMinor: position.source_entry_price_minor,
            issuanceCurrency: position.issuance_currency,
            referenceCurrency: position.reference_currency,
            issuedAt: issued_at,
            expiresAt: position.expires_at,
        },
    )?;

    Ok(position_id)
}

/// Burn `units` of the merchant's Issued Intex via `sendToGemFactory`
/// (GEM_ROLE) and return the burned count. Reverts if the series is in a
/// non-sendable (non-Issued) state or `units` is zero.
fn burn_intex_into_gem_factory(
    storage: &StorageHandle<'_>,
    owner: Address,
    series_id: SeriesId,
    units: U256,
) -> Result<U256> {
    let ret = storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::sendToGemFactoryCall {
            owner,
            seriesId: series_id.into(),
            units,
        }
        .abi_encode()
        .into(),
    )?;
    IIntexNFT1155::sendToGemFactoryCall::abi_decode_returns(&ret)
        .map_err(|_| GemFactoryError::UndecodableReturn("sendToGemFactory return").into())
}

/// Issue one Merchant gem to a customer, draining the position's capacity.
pub fn issue_merchant_gem(
    storage: &StorageHandle<'_>,
    params: crate::schema::MerchantGemIssueParams,
) -> Result<U256> {
    let crate::schema::MerchantGemIssueParams {
        caller,
        position_id,
        owner,
        promis_load_minor: promis_load,
    } = params;
    if owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }
    if promis_load.is_zero() {
        return Err(GemFactoryError::ZeroPromisLoad.into());
    }

    let mut factory = GemFactoryContract::new(storage.clone());
    let mut record = factory
        .positions
        .get(position_id)?
        .ok_or(GemFactoryError::PositionNotFound)?;
    if record.merchant != caller {
        return Err(GemFactoryError::NotPositionOwner.into());
    }

    let now = storage.now_secs()?;
    if now >= record.expires_at {
        return Err(GemFactoryError::PositionExpired.into());
    }
    let remaining = record
        .remaining_capacity_minor
        .checked_sub(promis_load)
        .ok_or(GemFactoryError::InsufficientCapacity)?;

    // An anti-dilution bound, not a price: never below the source Intex.
    let market_price = read_market_price(storage, record.reference_currency, now)?;
    let entry_price = market_price.max(record.source_entry_price_minor);

    let gem_id = add_gem(
        storage,
        GemAddParams {
            owner,
            promis_load_minor: promis_load,
            entry_price_minor: entry_price,
            issuance_currency: record.issuance_currency,
            reference_currency: record.reference_currency,
            issued_at: now,
            call_terms: None,
        },
    )?;

    record.remaining_capacity_minor = remaining;
    factory.positions.update(&record)?;
    factory.take_hour_capacity(record.expiry_hour(), promis_load)?;

    let prev_total = factory.total_gems_issued.read()?;
    let new_total = prev_total
        .checked_add(U256::from(1))
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gems_issued.write(new_total)?;

    emit_gem_issued(storage, gem_id, position_id)?;

    Ok(gem_id)
}

/// Settles a gem paying its cost from `payer` in `asset` by ERC20 transfer. Anyone
/// may pay for a gem. It stays with its owner. An issuance-currency payment must
/// name the VWAP snapshot required at this block.
pub fn settle_gem(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    payment: SettlementPayment,
) -> Result<()> {
    let SettlementPayment {
        payer,
        asset,
        snapshot_id,
    } = payment;
    let gem = load_view(storage, gem_id)?;
    require_settleable(storage, &gem)?;

    let quote = settlement::quote(storage, asset, currencies(&gem), &settlement_cost(&gem))?;
    quote.require_snapshot(snapshot_id)?;
    storage.clone().with_checkpoint(|| {
        // Settle before payment so a token callback cannot settle the gem twice.
        // A failed payment rolls the state back.
        let (item, group) =
            gem_api::load_pair(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
        gem_api::record_settled(storage, item, group)?;
        settlement::collect(
            storage,
            GEM_FACTORY_ADDRESS,
            payer,
            asset,
            quote.payment_minor,
        )?;
        emit_event(
            storage,
            GemSettled {
                gemId: gem_id,
                owner: gem.item.owner,
                asset,
                paymentMinor: quote.payment_minor,
                settlementCurrency: quote.settlement_currency,
            },
        )
    })
}

/// A gem the owner may pay for: an uncalled one, or a called one until its deadline.
pub(crate) fn require_settleable(storage: &StorageHandle<'_>, gem: &GemView) -> Result<()> {
    if gem.item.is_settled() {
        return Err(GemFactoryError::GemAlreadySettled.into());
    }
    if gem.group_called_at != 0 && storage.now_secs()? > gem.settlement_deadline() {
        return Err(GemFactoryError::SettlementDeadlinePassed.into());
    }
    Ok(())
}

/// Burns a settled gem for Promis mining.
pub(crate) fn burn_settled(storage: &StorageHandle<'_>, gem_id: U256) -> Result<()> {
    let (item, group) = gem_api::load_pair(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if !item.body().is_settled() {
        return Err(GemFactoryError::GemNotSettled.into());
    }
    gem_api::remove_gem(storage, item, group)
}

fn currencies(gem: &GemView) -> Currencies {
    Currencies {
        issuance: gem.item.issuance_currency,
        reference: gem.item.reference_currency,
    }
}

/// One gem: its entry price times its Promis load.
pub(crate) fn settlement_cost(gem: &GemView) -> SettlementCost {
    SettlementCost {
        entry_price_minor: gem.item.entry_price_minor,
        load_minor: gem.item.promis_load_minor,
        units: U256::ONE,
    }
}

#[cfg(test)]
pub(crate) fn settlement_units(
    gem: &GemView,
    rate: Option<(U256, U256)>,
    asset_decimals: u8,
) -> Result<U256> {
    outbe_primitives::settlement::settlement_cost_units(&settlement_cost(gem), rate, asset_decimals)
        .map_err(|e| settlement::SettlementError::Rounding(e).into())
}

/// What settlement charges in a six-decimal reference-currency asset.
#[cfg(test)]
pub(crate) fn gem_cost_minor(gem: &GemView) -> Result<U256> {
    settlement_units(gem, None, outbe_primitives::units::PROTOCOL_AMOUNT_DECIMALS)
}

/// What settling `gem_id` with `asset` costs, in which currency, and the VWAP
/// snapshot an issuance-currency payment must name (zero on the reference rail).
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    asset: Address,
) -> Result<(u16, U256, U256)> {
    let gem = load_view(storage, gem_id)?;
    let quote = settlement::quote(storage, asset, currencies(&gem), &settlement_cost(&gem))?;
    Ok((
        quote.settlement_currency,
        quote.payment_minor,
        quote.snapshot_id(),
    ))
}

/// The full terms of a Gem Factory position.
pub fn position_data(
    storage: &StorageHandle<'_>,
    position_id: U256,
) -> Result<crate::precompile::IGemFactory::PositionData> {
    let factory = GemFactoryContract::new(storage.clone());
    let record = factory
        .positions
        .get(position_id)?
        .ok_or(GemFactoryError::PositionNotFound)?;
    Ok(crate::precompile::IGemFactory::PositionData {
        positionId: record.position_id,
        merchant: record.merchant,
        sourceIntexId: record.source_intex_id.into(),
        remainingCapacityMinor: factory.remaining_capacity(&record)?,
        sourceEntryPriceMinor: record.source_entry_price_minor,
        issuanceCurrency: record.issuance_currency,
        referenceCurrency: record.reference_currency,
        issuedAt: record.issued_at,
        expiresAt: record.expires_at,
    })
}

pub fn mine_promis(
    storage: &StorageHandle<'_>,
    params: crate::schema::GemMiningProof,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    let crate::schema::GemMiningProof { gem_id, nonce } = params;
    let item = gem_api::get_item(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    // Anyone may submit. The owner's modify key authorizes the mint.
    if !item.is_settled() {
        return Err(GemFactoryError::GemNotSettled.into());
    }

    validate_pow(gem_id, item.owner, nonce)?;

    // Burn, mint and the mining event are one transition: a rejected or
    // failed mint leaves the settled Gem in place so the owner can retry.
    storage.clone().with_checkpoint(|| {
        burn_settled(storage, gem_id)?;

        // The Promis is confidential: the mint runs inside the enclave, authorized by
        // the gem owner's Promis modify key. The client's `mac`/`opNonce` must bind the
        // minted amount (`item.promis_load_minor`), so the client precomputes it.
        outbe_promisfactory::api::mint(storage.clone(), item.owner, item.promis_load_minor, auth)?;

        emit_event(
            storage,
            GemMined {
                gemId: gem_id,
                owner: item.owner,
                promisLoadMinor: item.promis_load_minor,
            },
        )?;

        Ok(item.promis_load_minor)
    })
}

/// COEN price of `iso_code` from the last closed UTC day.
fn read_market_price(storage: &StorageHandle<'_>, iso_code: u16, now: u64) -> Result<U256> {
    let day = previous_date_key(timestamp_to_date_key(now)).map_err(date_step_error)?;
    get_utc_day_vwap_for_iso(storage.clone(), day, iso_code)?
        .ok_or_else(|| GemFactoryError::OracleUnavailable.into())
}

pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(GEM_FACTORY_ADDRESS, event.encode_log_data())
}

/// PoW gate for `mine_promis`. The preimage is
/// `OUTBE_GEM_MINING_V1 || gemId || owner || miningSequence=0 || nonce`. The caller is not in it.
pub fn validate_pow(gem_id: U256, owner: Address, nonce: u64) -> Result<()> {
    pow::validate_mining_pow(
        pow::MiningDomain::Gem,
        gem_id,
        owner,
        pow::SINGLE_MINING_SEQUENCE,
        nonce,
    )
    .map_err(|e| GemFactoryError::from(e).into())
}
