use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gem::{api as gem_api, GemAddParams, GemState};
use outbe_intex::SeriesId;
use outbe_oracle::api::get_utc_day_vwap_for_iso;
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
use crate::precompile::IGemFactory::{GemExercised, GemIssued, GemSettled};
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
    let floor_price = compute_floor(gem_type, promis_load, entry_price, &terms)?;
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

    emit_event(
        storage,
        GemIssued {
            gemId: gem_id,
            gemType: gem_type as u8,
            owner,
            promisLoad: promis_load,
            entryPrice: entry_price,
            floorPrice: floor_price,
            issuanceCurrency: issuance_currency,
            referenceCurrency: reference_currency,
            issuedAt: issued_at,
        },
    )?;

    Ok(gem_id)
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
    factory.add_position(&GemPosition {
        position_id,
        merchant: caller,
        source_intex_id,
        remaining_capacity: capacity,
        source_entry_price: series.entry_price_minor,
        source_floor_price: series.floor_price_minor,
        issuance_currency: series.issuance_currency,
        reference_currency: series.reference_currency,
        issued_at,
        expires_at: issued_at.saturating_add(outbe_gem::config::read(storage)?.position_validity),
    })?;

    factory.push_live_position(position_id)?;

    let prev_sent = factory.total_gem_factory_units.read()?;
    let new_sent = prev_sent
        .checked_add(capacity)
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gem_factory_units.write(new_sent)?;

    Ok(position_id)
}

/// Burn `amount` of the merchant's Issued Intex units via `sendToGemFactory`
/// (GEM_ROLE) and return the burned count. Reverts if the series is in a
/// non-sendable (non-Issued/Qualified) state or `amount` is zero.
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
            amount,
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
        .remaining_capacity
        .checked_sub(promis_load)
        .ok_or(GemFactoryError::InsufficientCapacity)?;

    // Both maxima are an anti-dilution floor, not a price: never below the source Intex.
    let market_price = read_market_price(storage, record.reference_currency, now)?;
    let entry_price = market_price.max(record.source_entry_price);
    compute_cost(entry_price, promis_load, 100)?;
    let terms = outbe_gem::config::read(storage)?;
    let floor_price = derived_floor(entry_price, terms.floor_rate)?.max(record.source_floor_price);
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

    record.remaining_capacity = remaining;
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

    emit_event(
        storage,
        GemIssued {
            gemId: gem_id,
            gemType: GemTypes::Merchant as u8,
            owner,
            promisLoad: promis_load,
            entryPrice: entry_price,
            floorPrice: floor_price,
            issuanceCurrency: record.issuance_currency,
            referenceCurrency: record.reference_currency,
            issuedAt: now,
        },
    )?;

    Ok(gem_id)
}

/// Settles a gem paying its cost from `caller` in `asset` by direct ERC20 transfer.
pub fn settle_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    asset: Address,
) -> Result<()> {
    settle(storage, gem_id, |item| {
        let currency = accept_payment_asset(storage, asset, item)?;
        let amount_paid = cost_in_token(storage, item, asset, currency)?;
        deposit_payment(storage, caller, asset, amount_paid)?;
        Ok((settlement_currency(item, currency), amount_paid))
    })
}

/// Settles a gem by spending a PayNote owned by `caller`, so no tokens move here.
pub fn settle_gem_with_paynote(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    paynote_proof: &[u8],
) -> Result<()> {
    settle(storage, gem_id, |item| {
        let claim = outbe_paynote::api::consume(storage, paynote_proof)?;

        // Notes are bearer: anyone can relay a proof, so bind its owner to the caller.
        if claim.owner != caller {
            return Err(GemFactoryError::PayNoteOwnerMismatch {
                expected: caller,
                actual: claim.owner,
            }
            .into());
        }

        let currency = accept_payment_asset(storage, claim.asset, item)?;
        let amount_paid = cost_in_token(storage, item, claim.asset, currency)?;
        // Exact: the surplus of an over-spend is already in the reserve vault.
        if claim.spend_amount != amount_paid {
            return Err(GemFactoryError::PayNoteCostMismatch {
                covered: claim.spend_amount,
                required: amount_paid,
            }
            .into());
        }
        Ok((settlement_currency(item, currency), amount_paid))
    })
}

/// `pay` returns the settlement currency and the amount it charged.
fn settle(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    pay: impl FnOnce(&outbe_gem::GemData) -> Result<(u16, U256)>,
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
        s if (s == GemState::Issued as u8 || s == GemState::Qualified as u8)
            && gem_api::is_qualified(storage, &item)? => {}
        _ => return Err(GemFactoryError::InvalidState.into()),
    }

    storage.clone().with_checkpoint(|| {
        // Settled before payment so a token callback cannot settle the gem twice;
        // a failed payment rolls the state back.
        gem_api::set_state(storage, gem_id, GemState::Settled)?;
        let (settlement_currency, amount_paid) = pay(&item)?;
        emit_event(
            storage,
            GemSettled {
                gemId: gem_id,
                owner: item.owner,
                amountPaid: amount_paid,
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

/// Cost of one gem in `asset`'s minor units. The issuance rail folds the COEN
/// cross rate of the last closed UTC day into the same fraction, so the whole
/// thing is floored once.
fn cost_in_token(
    storage: &StorageHandle<'_>,
    item: &outbe_gem::GemData,
    asset: Address,
    currency: PaymentCurrency,
) -> Result<U256> {
    let asset_decimals = read_decimals(storage, asset)?;
    let rate = match currency {
        PaymentCurrency::Reference => None,
        PaymentCurrency::Issuance => {
            // One timestamp, so both legs come from the same closed day.
            let now = storage.timestamp()?.to::<u64>();
            Some((
                read_market_price(storage, item.issuance_currency, now)?,
                read_market_price(storage, item.reference_currency, now)?,
            ))
        }
    };
    settlement_units(item, rate, asset_decimals)
}

/// `floor(entry x load x percent x rate_to / (100 x rate_from))` in asset units,
/// with `rate` as `(COEN/issuance, COEN/reference)` on the issuance rail.
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

/// The gem's cost in its reference currency at six decimals: the formula the
/// issuance guard applies. Settlement does not floor here.
#[cfg(test)]
pub(crate) fn gem_cost_minor(item: &outbe_gem::GemData) -> Result<U256> {
    compute_cost(
        item.entry_price_minor,
        item.promis_load_minor,
        cost_rate(item.gem_type),
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

/// What settling `gem_id` with `asset` costs, and in which currency.
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    asset: Address,
) -> Result<(u16, U256)> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    let currency = accept_payment_asset(storage, asset, &item)?;
    Ok((
        settlement_currency(&item, currency),
        cost_in_token(storage, &item, asset, currency)?,
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
        remainingCapacity: record.remaining_capacity,
        sourceEntryPrice: record.source_entry_price,
        sourceFloorPrice: record.source_floor_price,
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
            promisLoad: item.promis_load_minor,
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
    promis_load: U256,
    coen_rate: U256,
    terms: &outbe_gem::GemParams,
) -> Result<U256> {
    // The cost is derived from the record on demand; it is computed here only to
    // reject a load whose cost rounds to zero.
    let floor_price = match gem_type {
        // A zero floor qualifies the gem from birth: every price clears it.
        GemTypes::Genesis => {
            compute_cost(coen_rate, promis_load, 100)?;
            U256::ZERO
        }
        GemTypes::Sra => {
            compute_cost(coen_rate, promis_load, SRA_RATE)?;
            derived_floor(coen_rate, terms.floor_rate)?
        }
        // Validator (post-genesis), Wallet, Cca - standard agent-class flow:
        // cost = entry x load, floor = rate x 1.08, born Issued.
        GemTypes::Validator | GemTypes::Wallet | GemTypes::Cca => {
            compute_cost(coen_rate, promis_load, 100)?;
            derived_floor(coen_rate, terms.floor_rate)?
        }
        // Merchant gems are issued via `issue_merchant_gem` against a GemPosition,
        // not through this agent-class path.
        GemTypes::Merchant => return Err(GemFactoryError::UnsupportedGemType.into()),
    };
    Ok(floor_price)
}

/// `floor(entry x load x percent / (100 x SCALE_1E6_U256))`. Entry, load and
/// result are six-decimal monetary values; the calculation rounds only once.
fn compute_cost(entry: U256, load: U256, cost_num: u64) -> Result<U256> {
    let numerator = entry
        .checked_mul(load)
        .ok_or(GemFactoryError::Overflow)?
        .checked_mul(U256::from(cost_num))
        .ok_or(GemFactoryError::Overflow)?;
    let denominator = SCALE_1E6_U256
        .checked_mul(U256::from(100u64))
        .ok_or(GemFactoryError::Overflow)?;
    let cost = numerator / denominator;
    if !entry.is_zero() && !load.is_zero() && cost.is_zero() {
        return Err(PrecompileError::Revert(
            "gem cost rounds to zero".to_owned(),
        ));
    }
    Ok(cost)
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
/// `gemId || owner || miningSequence=0 || nonce`; the caller is not in it.
pub fn validate_pow(gem_id: U256, owner: Address, nonce: u64) -> Result<()> {
    pow::validate_mining_pow(gem_id, owner, pow::SINGLE_EXERCISE_SEQUENCE, nonce)
        .map_err(|e| GemFactoryError::from(e).into())
}
