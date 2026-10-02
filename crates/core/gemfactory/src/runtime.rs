use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gem::{api as gem_api, GemAddParams, GemState};
use outbe_intex::SeriesId;
use outbe_oracle::api::{get_utc_day_vwap_for_iso, settlement_fx_rates, VwapSnapshotId};
use outbe_primitives::addresses::{
    GEM_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS, VAULT_ROUTER_ADDRESS,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
use outbe_primitives::units::SCALE_1E6_U256;

use outbe_common::pow;
use outbe_common::settlement::floor_to_asset_units;

use crate::constants::SRA_RATE;
use crate::errors::GemFactoryError;
use crate::precompile::IGemFactory::{GemExercised, GemIssued, GemPositionIssued, GemSettled};
use crate::schema::{GemFactoryContract, GemPosition, GemTypes};
use crate::sol_ext::{IIntexNFT1155, IReferenceCurrency, IERC20};
use outbe_vaultrouter::api::IVaultRouter;

/// Issues one agent-class gem priced at `entry_price`, the COEN rate in
/// `reference_currency` that the caller resolved for the gem's own day.
pub fn issue_gem(
    storage: &StorageHandle<'_>,
    owner: Address,
    gem_type: GemTypes,
    promis_load: U256,
    issuance_currency: u16,
    reference_currency: u16,
    entry_price: U256,
) -> Result<U256> {
    if owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }
    if entry_price.is_zero() {
        return Err(GemFactoryError::OracleUnavailable.into());
    }
    // A zero load makes the cost zero, and a PayNote cannot spend zero.
    if promis_load.is_zero() {
        return Err(GemFactoryError::ZeroPromisLoad.into());
    }

    // The owner's own label: only its range is checked, as the auction checks a bid's.
    if issuance_currency == 0 || issuance_currency > 999 {
        return Err(GemFactoryError::InvalidCurrency {
            currency: issuance_currency,
        }
        .into());
    }
    outbe_oracle::api::check_reference_currency_with_storage(storage.clone(), reference_currency)?;

    // The caller resolves the price: it knows which day the gem belongs to.
    let issued_at = storage.timestamp()?.to::<u64>();
    let terms = outbe_gem::config::read(storage)?;
    let floor_price = compute_floor(gem_type, entry_price, &terms)?;
    let call_price = derived_call_price(entry_price, terms.call_rate)?;

    let params = GemAddParams {
        owner,
        gem_type: gem_type as u8,
        promis_load_minor: promis_load,
        entry_price_minor: entry_price,
        floor_price_minor: floor_price,
        call_price_minor: call_price,
        call_rate: terms.call_rate,
        issuance_currency,
        reference_currency,
        issued_at,
    };
    let gem_id = gem_api::add_gem(storage, params)?;

    let factory = GemFactoryContract::new(storage.clone());
    let prev_total = factory.total_gems_issued.read()?;
    let new_total = prev_total
        .checked_add(U256::from(1))
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gems_issued.write(new_total)?;

    emit_gem_issued(storage, gem_id, U256::ZERO)?;

    Ok(gem_id)
}

/// Announce a new gem with the terms its record and call bucket were given.
fn emit_gem_issued(storage: &StorageHandle<'_>, gem_id: U256, position_id: U256) -> Result<()> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    emit_event(
        storage,
        GemIssued {
            gemId: gem_id,
            gemType: item.gem_type,
            owner: item.owner,
            promisLoadMinor: item.promis_load_minor,
            entryPriceMinor: item.entry_price_minor,
            floorPriceMinor: item.floor_price_minor,
            issuanceCurrency: item.issuance_currency,
            referenceCurrency: item.reference_currency,
            issuedAt: item.issued_at,
            callPriceMinor: item.call_price_minor,
            callWindow: item.call_window_seconds,
            callThreshold: item.call_threshold_seconds,
            callNoticePeriod: item.call_notice_period_seconds,
            positionId: position_id,
            bucketKey: gem_api::bucket_of(storage, gem_id)?,
        },
    )
}

/// Send a merchant's whole Intex series to the Gem Factory and issue a GemPosition NFT. Burns the
/// merchant's entire Issued holding on IntexNFT1155 (`sendToGemFactory`, GEM_ROLE)
/// and records the position with a snapshot of the source entry/floor and the
/// resulting Promis capacity. Returns the issued `position_id`.
pub fn issue_gem_position(
    storage: &StorageHandle<'_>,
    caller: Address,
    source_intex_id: SeriesId,
    amount: U256,
) -> Result<U256> {
    if caller.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }

    let series = outbe_intex::api::get_series(storage, source_intex_id)?
        .ok_or(GemFactoryError::SourceIntexNotFound)?;

    // The daily call scan walks only listed reference currencies.
    outbe_oracle::api::check_reference_currency_with_storage(
        storage.clone(),
        series.reference_currency,
    )?;

    // Burn `amount` of the merchant's Intex units; `sendToGemFactory` returns the
    // burned count (and reverts on a state that may not be sent, or a zero amount).
    let units = burn_intex_into_gem_factory(storage, caller, source_intex_id, amount)?;
    let capacity = series
        .promis_load_minor
        .checked_mul(units)
        .ok_or(GemFactoryError::Overflow)?;

    // Their load moved into the position, so the source series cannot forfeit them.
    let gem_factory_units = u32::try_from(units).map_err(|_| GemFactoryError::Overflow)?;
    outbe_intex::api::record_gem_factory_units(
        storage,
        source_intex_id,
        caller,
        gem_factory_units,
    )?;

    let issued_at = storage.timestamp()?.to::<u64>();
    let position_id =
        GemFactoryContract::generate_position_id(caller, source_intex_id, storage.block_number()?);

    let mut factory = GemFactoryContract::new(storage.clone());
    let position = GemPosition {
        position_id,
        merchant: caller,
        source_intex_id,
        remaining_capacity_minor: capacity,
        source_entry_price_minor: series.entry_price_minor,
        source_floor_price_minor: series.floor_price_minor,
        issuance_currency: series.issuance_currency,
        reference_currency: series.reference_currency,
        issued_at,
        expires_at: issued_at.saturating_add(outbe_gem::config::read(storage)?.position_validity),
    };
    factory.add_position(&position)?;

    factory.push_live_position(position_id)?;

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
            sourceFloorPriceMinor: position.source_floor_price_minor,
            issuanceCurrency: position.issuance_currency,
            referenceCurrency: position.reference_currency,
            issuedAt: issued_at,
            expiresAt: position.expires_at,
        },
    )?;

    Ok(position_id)
}

/// Burn `amount` of the merchant's Issued Intex units via `sendToGemFactory`
/// (GEM_ROLE) and return the burned count. Reverts if the series is in a
/// non-sendable (non-Issued) state or `amount` is zero.
fn burn_intex_into_gem_factory(
    storage: &StorageHandle<'_>,
    owner: Address,
    series_id: SeriesId,
    amount: U256,
) -> Result<U256> {
    let ret = storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::sendToGemFactoryCall {
            owner,
            seriesId: series_id.into(),
            units: amount,
        }
        .abi_encode()
        .into(),
    )?;
    IIntexNFT1155::sendToGemFactoryCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("sendToGemFactory return undecodable".into()))
}

/// Issue one Merchant gem to a customer, draining the position's capacity.
pub fn issue_merchant_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    owner: Address,
    promis_load: U256,
) -> Result<U256> {
    if owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }
    // A zero load makes the cost zero, and a PayNote cannot spend zero.
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

    let now = storage.timestamp()?.to::<u64>();
    if now >= record.expires_at {
        return Err(GemFactoryError::PositionExpired.into());
    }
    let remaining = record
        .remaining_capacity_minor
        .checked_sub(promis_load)
        .ok_or(GemFactoryError::InsufficientCapacity)?;

    // Both maxima are an anti-dilution floor, not a price: never below the source Intex.
    let market_price = read_market_price(storage, record.reference_currency, now)?;
    let entry_price = market_price.max(record.source_entry_price_minor);
    let terms = outbe_gem::config::read(storage)?;
    let floor_price =
        derived_floor(entry_price, terms.floor_rate)?.max(record.source_floor_price_minor);
    let call_price = derived_call_price(entry_price, terms.call_rate)?;

    let gem_id = gem_api::add_gem(
        storage,
        GemAddParams {
            owner,
            gem_type: GemTypes::Merchant as u8,
            promis_load_minor: promis_load,
            entry_price_minor: entry_price,
            floor_price_minor: floor_price,
            call_price_minor: call_price,
            call_rate: terms.call_rate,
            issuance_currency: record.issuance_currency,
            reference_currency: record.reference_currency,
            issued_at: now,
        },
    )?;

    record.remaining_capacity_minor = remaining;
    factory.positions.update(&record)?;
    // Nothing left to return: it leaves the queue instead of sitting at the head.
    if remaining.is_zero() {
        factory.remove_live_position(position_id)?;
    }

    let prev_total = factory.total_gems_issued.read()?;
    let new_total = prev_total
        .checked_add(U256::from(1))
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gems_issued.write(new_total)?;

    emit_gem_issued(storage, gem_id, position_id)?;

    Ok(gem_id)
}

/// Settles a gem paying its cost from `caller` in `asset` by direct ERC20 transfer.
/// An issuance-currency payment must name the VWAP snapshot required at this block.
pub fn settle_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    asset: Address,
    snapshot_id: U256,
) -> Result<()> {
    let quote = |item: &outbe_gem::GemData| {
        let currency = accept_payment_asset(storage, asset, item)?;
        let (amount_paid, snapshot) = cost_in_token(storage, item, asset, currency)?;
        require_snapshot(snapshot, snapshot_id)?;
        Ok((settlement_currency(item, currency), amount_paid))
    };
    settle(
        storage,
        gem_id,
        quote,
        |_, (settlement_currency, amount_paid)| {
            deposit_payment(storage, caller, asset, amount_paid)?;
            Ok((asset, settlement_currency, amount_paid))
        },
    )
}

/// Settles a gem by spending a PayNote bound to this gem. Any address may relay it.
pub fn settle_gem_with_paynote(
    storage: &StorageHandle<'_>,
    _caller: Address,
    gem_id: U256,
    paynote_proof: &[u8],
) -> Result<()> {
    settle(
        storage,
        gem_id,
        |_| Ok(()),
        |item, ()| {
            let claim = outbe_paynote::api::consume(storage, paynote_proof)?;
            let currency = accept_payment_asset(storage, claim.asset, item)?;
            let (amount_paid, snapshot) = cost_in_token(storage, item, claim.asset, currency)?;
            let expected = outbe_paynote::api::settlement_context(
                outbe_paynote::api::SettlementDomain::Gem,
                B256::from(gem_id),
                U256::ONE,
                snapshot.map_or(U256::ZERO, VwapSnapshotId::to_u256),
            )?;
            if claim.context != expected {
                return Err(GemFactoryError::PayNoteContextMismatch {
                    expected,
                    actual: claim.context,
                }
                .into());
            }
            // Exact: the surplus of an over-spend is already in the reserve vault.
            if claim.spend_amount != amount_paid {
                return Err(GemFactoryError::PayNoteCostMismatch {
                    covered: claim.spend_amount,
                    required: amount_paid,
                }
                .into());
            }
            Ok((
                claim.asset,
                settlement_currency(item, currency),
                amount_paid,
            ))
        },
    )
}

/// `quote` prices and authorizes the payment before any state changes; `pay`
/// then moves it after the transition and returns the asset, the settlement
/// currency and the amount it charged.
fn settle<Q>(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    quote: impl FnOnce(&outbe_gem::GemData) -> Result<Q>,
    pay: impl FnOnce(&outbe_gem::GemData, Q) -> Result<(Address, u16, U256)>,
) -> Result<()> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    // Anyone may pay for a gem; the payment is bound to the caller, the gem is not.
    // The qualification walk goes last.
    match item.state {
        s if s == GemState::Called as u8 => {
            let now = storage.timestamp()?.to::<u64>();
            let deadline = item.called_at + u64::from(item.call_notice_period_seconds);
            if now > deadline {
                return Err(GemFactoryError::DeadlineExpired.into());
            }
        }
        s if s == GemState::Issued as u8 && gem_api::is_qualified(storage, &item)? => {}
        _ => return Err(GemFactoryError::InvalidState.into()),
    }

    let quoted = quote(&item)?;
    storage.clone().with_checkpoint(|| {
        // Settled before payment so a token callback cannot settle the gem twice;
        // a failed payment rolls the state back.
        gem_api::set_state(storage, gem_id, GemState::Settled)?;
        let (asset, settlement_currency, amount_paid) = pay(&item, quoted)?;
        emit_event(
            storage,
            GemSettled {
                gemId: gem_id,
                owner: item.owner,
                asset,
                paymentMinor: amount_paid,
                settlementCurrency: settlement_currency,
            },
        )
    })
}

fn settlement_currency(item: &outbe_gem::GemData, currency: PaymentCurrency) -> u16 {
    match currency {
        PaymentCurrency::Reference => item.reference_currency,
        PaymentCurrency::Issuance => item.issuance_currency,
    }
}

/// Pulls exactly `cost` of `asset` from `payer` and deposits it into the reserve
/// vault through the router, leaving the factory's own balance untouched.
fn deposit_payment(
    storage: &StorageHandle<'_>,
    payer: Address,
    asset: Address,
    cost: U256,
) -> Result<()> {
    if cost.is_zero() {
        return Ok(());
    }
    let before = token_balance(storage, asset)?;
    checked_token_call(
        storage,
        asset,
        IERC20::transferFromCall {
            from: payer,
            to: GEM_FACTORY_ADDRESS,
            amount: cost,
        },
    )?;
    if token_balance(storage, asset)?.checked_sub(before) != Some(cost) {
        return Err(GemFactoryError::SettlementAmountMismatch.into());
    }
    checked_token_call(
        storage,
        asset,
        IERC20::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount: cost,
        },
    )?;
    outbe_vaultrouter::api::deposit(storage, asset, cost)?;
    if token_balance(storage, asset)? != before {
        return Err(GemFactoryError::SettlementAmountMismatch.into());
    }
    Ok(())
}

fn checked_token_call(
    storage: &StorageHandle<'_>,
    asset: Address,
    call: impl SolCall,
) -> Result<()> {
    let ret = storage.call(asset, U256::ZERO, call.abi_encode().into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(GemFactoryError::TokenOperationFailed.into());
    }
    Ok(())
}

fn token_balance(storage: &StorageHandle<'_>, asset: Address) -> Result<U256> {
    let ret = storage.staticcall(
        asset,
        IERC20::balanceOfCall {
            account: GEM_FACTORY_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| GemFactoryError::TokenOperationFailed.into())
}

/// Reads the settlement asset's `decimals()` via a static sub-call.
fn read_decimals(storage: &StorageHandle<'_>, asset: Address) -> Result<u8> {
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret).map_err(|_| GemFactoryError::InvalidAsset.into())
}

/// Which of a gem's two currencies a payment asset is denominated in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaymentCurrency {
    Reference,
    Issuance,
}

/// Which of the gem's two currencies `asset` is denominated in. Registration is
/// checked first, so an unregistered asset need not implement `isoCode()` at all;
/// reference is matched first, so a single-currency gem takes the no-rate branch.
fn accept_payment_asset(
    storage: &StorageHandle<'_>,
    asset: Address,
    item: &outbe_gem::GemData,
) -> Result<PaymentCurrency> {
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall { asset }
            .abi_encode()
            .into(),
    )?;
    let vaults = IVaultRouter::assetVaultsCountCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("assetVaultsCount undecodable".into()))?;
    if vaults.is_zero() {
        return Err(GemFactoryError::SettlementAssetNotRegistered { asset }.into());
    }

    let iso = asset_iso_code(storage, asset)?;
    if iso == item.reference_currency {
        return Ok(PaymentCurrency::Reference);
    }
    if iso == item.issuance_currency {
        return Ok(PaymentCurrency::Issuance);
    }
    Err(GemFactoryError::SettlementCurrencyMismatch { iso_code: iso }.into())
}

/// Cost of one gem in `asset`'s minor units and, on the issuance rail, the VWAP
/// snapshot both COEN legs came from. The cross rate is folded into the same
/// fraction, so the whole thing is floored once.
fn cost_in_token(
    storage: &StorageHandle<'_>,
    item: &outbe_gem::GemData,
    asset: Address,
    currency: PaymentCurrency,
) -> Result<(U256, Option<VwapSnapshotId>)> {
    let asset_decimals = read_decimals(storage, asset)?;
    let (rate, snapshot) = match currency {
        PaymentCurrency::Reference => (None, None),
        PaymentCurrency::Issuance => {
            let fx = settlement_fx_rates(
                storage.clone(),
                item.issuance_currency,
                item.reference_currency,
            )?
            .ok_or(GemFactoryError::OracleUnavailable)?;
            let rate = (
                fx.issuance_currency_vwap_minor,
                fx.reference_currency_vwap_minor,
            );
            (Some(rate), Some(fx.snapshot))
        }
    };
    Ok((settlement_units(item, rate, asset_decimals)?, snapshot))
}

/// Rejects an issuance-rail payment authorized for any snapshot but the required one.
fn require_snapshot(required: Option<VwapSnapshotId>, authorized: U256) -> Result<()> {
    match required.map(VwapSnapshotId::to_u256) {
        Some(required) if required != authorized => Err(GemFactoryError::VwapSnapshotMismatch {
            authorized,
            required,
        }
        .into()),
        _ => Ok(()),
    }
}

/// `floor(entry x load x percent x rate_to / (100 x rate_from))` in asset units,
/// with `rate` as `(COEN/issuance, COEN/reference)` on the issuance rail. A
/// positive cost is at least one reference-currency minor unit before conversion.
pub(crate) fn settlement_units(
    item: &outbe_gem::GemData,
    rate: Option<(U256, U256)>,
    asset_decimals: u8,
) -> Result<U256> {
    const OBLIGATION_DECIMALS: u32 = 12;
    let percent = U256::from(100u64);
    let obligation = item
        .entry_price_minor
        .checked_mul(item.promis_load_minor)
        .ok_or(GemFactoryError::Overflow)?
        .checked_mul(U256::from(cost_rate(item.gem_type)))
        .ok_or(GemFactoryError::Overflow)?;
    // The obligation keeps the x100 of the rate: one reference minor unit is 1e8.
    let obligation = if obligation.is_zero() {
        obligation
    } else {
        obligation.max(SCALE_1E6_U256 * percent)
    };
    let (numerator, denominator) = match rate {
        Some((to, from)) => (
            obligation
                .checked_mul(to)
                .ok_or(GemFactoryError::Overflow)?,
            from.checked_mul(percent).ok_or(GemFactoryError::Overflow)?,
        ),
        None => (obligation, percent),
    };
    floor_to_asset_units(numerator, denominator, OBLIGATION_DECIMALS, asset_decimals)
        .map_err(|e| GemFactoryError::from(e).into())
}

/// What settlement charges in a six-decimal reference-currency asset.
#[cfg(test)]
pub(crate) fn gem_cost_minor(item: &outbe_gem::GemData) -> Result<U256> {
    settlement_units(
        item,
        None,
        outbe_primitives::units::PROTOCOL_AMOUNT_DECIMALS,
    )
}

/// Share of the full agent cost this gem type pays, in percent.
fn cost_rate(gem_type: u8) -> u64 {
    if gem_type == GemTypes::Sra as u8 {
        SRA_RATE
    } else {
        100
    }
}

/// Reads the settlement asset's ISO 4217 code via a static sub-call.
fn asset_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("isoCode undecodable".into()))
}

/// What settling `gem_id` with `asset` costs, in which currency, and the VWAP
/// snapshot an issuance-currency payment must name (zero on the reference rail).
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    asset: Address,
) -> Result<(u16, U256, U256)> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    let currency = accept_payment_asset(storage, asset, &item)?;
    let (cost, snapshot) = cost_in_token(storage, &item, asset, currency)?;
    Ok((
        settlement_currency(&item, currency),
        cost,
        snapshot.map_or(U256::ZERO, VwapSnapshotId::to_u256),
    ))
}

/// The full terms of a Gem Factory position.
pub fn position_data(
    storage: &StorageHandle<'_>,
    position_id: U256,
) -> Result<crate::precompile::IGemFactory::PositionData> {
    let record = GemFactoryContract::new(storage.clone())
        .positions
        .get(position_id)?
        .ok_or(GemFactoryError::PositionNotFound)?;
    Ok(crate::precompile::IGemFactory::PositionData {
        positionId: record.position_id,
        merchant: record.merchant,
        sourceIntexId: record.source_intex_id.into(),
        remainingCapacityMinor: record.remaining_capacity_minor,
        sourceEntryPriceMinor: record.source_entry_price_minor,
        sourceFloorPriceMinor: record.source_floor_price_minor,
        issuanceCurrency: record.issuance_currency,
        referenceCurrency: record.reference_currency,
        issuedAt: record.issued_at,
        expiresAt: record.expires_at,
    })
}

pub fn mine_promis(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    nonce: u64,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    // Anyone may submit; the owner's modify key authorizes the mint.
    if item.state != GemState::Settled as u8 {
        return Err(GemFactoryError::InvalidState.into());
    }

    validate_pow(gem_id, item.owner, nonce)?;

    gem_api::burn(storage, gem_id)?;

    // The Promis is confidential: the mint runs inside the enclave, authorized by
    // the gem owner's Promis modify key. The client's `mac`/`opNonce` must bind the
    // minted amount (`item.promis_load_minor`), so the client precomputes it.
    outbe_promisfactory::api::mint(storage.clone(), item.owner, item.promis_load_minor, auth)?;

    emit_event(
        storage,
        GemExercised {
            gemId: gem_id,
            owner: item.owner,
            promisLoadMinor: item.promis_load_minor,
        },
    )?;

    Ok(item.promis_load_minor)
}

/// COEN price of `iso_code` from the last closed UTC day.
fn read_market_price(storage: &StorageHandle<'_>, iso_code: u16, now: u64) -> Result<U256> {
    let day = previous_date_key(timestamp_to_date_key(now));
    get_utc_day_vwap_for_iso(storage.clone(), day, iso_code)?
        .ok_or_else(|| GemFactoryError::OracleUnavailable.into())
}

fn compute_floor(
    gem_type: GemTypes,
    coen_rate: U256,
    terms: &outbe_gem::GemParams,
) -> Result<U256> {
    let floor_price = match gem_type {
        // A zero floor clears at any price on the gem's first full day.
        GemTypes::Genesis => U256::ZERO,
        // Every other agent class: floor = rate x 1.08.
        GemTypes::Sra | GemTypes::Validator | GemTypes::Wallet | GemTypes::Cca => {
            derived_floor(coen_rate, terms.floor_rate)?
        }
        // Merchant gems are issued via `issue_merchant_gem` against a GemPosition,
        // not through this agent-class path.
        GemTypes::Merchant => return Err(GemFactoryError::UnsupportedGemType.into()),
    };
    Ok(floor_price)
}

/// Floor price = `entry x (100 + FLOOR_RATE) / 100` (8% markup => 1.08x).
fn derived_floor(entry_price: U256, floor_rate: u16) -> Result<U256> {
    let acc = entry_price
        .checked_mul(U256::from(100 + u64::from(floor_rate)))
        .ok_or(GemFactoryError::Overflow)?;
    Ok(acc / U256::from(100u64))
}

/// Call price = `entry x (100 + CALL_RATE) / 100` (128% markup => 2.28x).
/// Entry equals the issuance-time coen rate in the single-currency case.
fn derived_call_price(entry_price: U256, call_rate: u16) -> Result<U256> {
    let acc = entry_price
        .checked_mul(U256::from(100 + u64::from(call_rate)))
        .ok_or(GemFactoryError::Overflow)?;
    Ok(acc / U256::from(100u64))
}

pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(GEM_FACTORY_ADDRESS, event.encode_log_data())
}

/// PoW gate for `mine_promis`. The preimage is
/// `OUTBE_GEM_MINING_V1 || gemId || owner || miningSequence=0 || nonce`; the caller is not in it.
pub fn validate_pow(gem_id: U256, owner: Address, nonce: u64) -> Result<()> {
    pow::validate_mining_pow(
        pow::MiningDomain::Gem,
        gem_id,
        owner,
        pow::SINGLE_EXERCISE_SEQUENCE,
        nonce,
    )
    .map_err(|e| GemFactoryError::from(e).into())
}
