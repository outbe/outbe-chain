use super::*;

/// Cost of `units` in payment-asset minor units, floored once above the
/// purchase minimum of one reference-currency minor unit. `rate` is
/// `(COEN/target, COEN/reference)` when the asset is not in the reference currency.
pub(crate) fn settlement_units(
    product: U256,
    units: U256,
    rate: Option<(U256, U256)>,
    payment_decimals: u8,
) -> Result<U256> {
    let overflow = || PrecompileError::Revert("settlement cost overflow".into());
    let obligation = product.checked_mul(units).ok_or_else(overflow)?;
    // A priceless series stays free.
    let obligation = if obligation.is_zero() {
        obligation
    } else {
        obligation.max(SCALE_1E6_U256)
    };
    let (numerator, denominator) = match rate {
        Some((to, from)) => (obligation.checked_mul(to).ok_or_else(overflow)?, from),
        None => (obligation, U256::ONE),
    };
    floor_to_asset_units(numerator, denominator, PRODUCT_DECIMALS, payment_decimals)
        .map_err(|e| IntexFactoryError::from(e).into())
}

/// Whether the series has qualified; derived from finalized daily VWAPs, never stored.
pub fn is_qualified(
    storage: &StorageHandle<'_>,
    series: &outbe_intex::SeriesRecord,
) -> Result<bool> {
    outbe_intex::api::is_qualified(storage, series)
}

pub fn is_series_qualified(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<bool> {
    is_qualified(storage, &outbe_intex::api::read_series(storage, series_id)?)
}

/// What settling `units` of `series_id` with `asset` costs, which
/// of the series' two currencies that asset settles on, and the VWAP snapshot an
/// issuance-currency payment must name (zero on the reference rail). Priced exactly
/// as `settleIntex` charges it. Rejects an asset the series does not accept.
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    asset: Address,
    units: U256,
) -> Result<(u16, U256, U256)> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    let currency = accept_payment_asset(storage, asset, &series)?;
    let settlement_currency = match currency {
        PaymentCurrency::Reference => series.reference_currency,
        PaymentCurrency::Issuance => series.issuance_currency,
    };
    let (cost, snapshot) = cost_in_asset(storage, &series, asset, currency, units)?;
    Ok((
        settlement_currency,
        cost,
        snapshot.map_or(U256::ZERO, VwapSnapshotId::to_u256),
    ))
}

/// Which of the series' two currencies a payment asset is denominated in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PaymentCurrency {
    Reference,
    Issuance,
}

/// Cost of `units` in `asset`'s minor units and, on the issuance rail, the
/// VWAP snapshot both COEN legs came from. The Cost Amount is denominated in the
/// reference currency; an issuance-currency asset is charged at the snapshot's
/// COEN cross rate, folded into the same fraction so the conversion floors once.
pub(super) fn cost_in_asset(
    storage: &StorageHandle<'_>,
    series: &outbe_intex::SeriesRecord,
    asset: Address,
    currency: PaymentCurrency,
    units: U256,
) -> Result<(U256, Option<VwapSnapshotId>)> {
    let payment_decimals = erc20_decimals(storage, asset)?;
    let product = series
        .entry_price_minor
        .checked_mul(series.promis_load_minor)
        .ok_or_else(|| PrecompileError::Revert("cost amount overflow".into()))?;
    let target_iso = match currency {
        PaymentCurrency::Reference => series.reference_currency,
        PaymentCurrency::Issuance => series.issuance_currency,
    };
    let (rate, snapshot) = if target_iso == series.reference_currency {
        (None, None)
    } else {
        let fx = settlement_fx_rates(storage.clone(), target_iso, series.reference_currency)?
            .ok_or(IntexFactoryError::OracleUnavailable)?;
        let rate = (
            fx.issuance_currency_vwap_minor,
            fx.reference_currency_vwap_minor,
        );
        (Some(rate), Some(fx.snapshot))
    };
    Ok((
        settlement_units(product, units, rate, payment_decimals)?,
        snapshot,
    ))
}

/// Rejects `asset` unless the router holds a vault for it and the asset reports
/// one of the series' two currencies; returns which one. Registration is checked
/// first: an unregistered asset need not implement `isoCode()` at all.
pub(super) fn accept_payment_asset(
    storage: &StorageHandle<'_>,
    asset: Address,
    series: &outbe_intex::SeriesRecord,
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
        return Err(IntexFactoryError::SettlementAssetNotRegistered(asset).into());
    }

    let iso = asset_iso_code(storage, asset)?;
    if iso == series.reference_currency {
        return Ok(PaymentCurrency::Reference);
    }
    if iso == series.issuance_currency {
        return Ok(PaymentCurrency::Issuance);
    }
    Err(IntexFactoryError::SettlementCurrencyMismatch(iso).into())
}

fn asset_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("isoCode undecodable".into()))
}

fn erc20_decimals(storage: &StorageHandle<'_>, asset: Address) -> Result<u8> {
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("ERC20 decimals undecodable".into()))
}
