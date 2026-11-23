//! Quote once, before withdrawing the reserved amount. Issuance never refreshes these terms.
use crate::{
    errors::{date_step_error, VaultRouterError},
    schema::LiquidityReservation,
    sol_ext::{IReferenceCurrency, IERC20},
};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_core_common::series_id::SeriesId;
use outbe_credis::CredisContract;
use outbe_oracle::api::{self, current_vwap_24h_snapshot, vwap_24h};
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::{first_full_day, timestamp_to_date_key};
use outbe_system_common::{math::scaled_math::checked_quote, units::MAX_ASSET_DECIMALS};
pub(crate) fn quote(
    storage: &StorageHandle<'_>,
    asset: Address,
    amount: U256,
    reference_currency: u16,
) -> Result<LiquidityReservation> {
    if storage.with_account_info(asset, |info| Ok(info.is_empty_code_hash()))? {
        return Err(VaultRouterError::InvalidAsset.into());
    }
    let data = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    let decimals = IERC20::decimalsCall::abi_decode_returns_validate(&data)
        .map_err(|_| VaultRouterError::InvalidAssetDecimals)?;
    if decimals > MAX_ASSET_DECIMALS {
        return Err(VaultRouterError::UnsupportedAssetDecimalsSimple.into());
    }
    let data = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    let currency = IReferenceCurrency::isoCodeCall::abi_decode_returns_validate(&data)
        .map_err(|_| VaultRouterError::InvalidAssetCurrency)?;
    api::check_reference_currency(reference_currency)?;
    let snapshot = current_vwap_24h_snapshot(storage.clone())?;
    let valuation = vwap_24h(storage.clone(), currency, snapshot)?
        .filter(|v| !v.is_zero())
        .ok_or(VaultRouterError::PledgePriceUnavailable)?;
    let entry_price_minor = vwap_24h(storage.clone(), reference_currency, snapshot)?
        .filter(|v| !v.is_zero())
        .ok_or(VaultRouterError::ReferencePriceUnavailable)?;
    let gratis_minor = checked_quote(amount, decimals, valuation)?;
    let call_terms = outbe_credis::config::read(storage)?;
    let now = storage.now_secs()?;
    let (policy_rate, published_at) = api::get_policy_rate(storage.clone(), currency)?;
    if now.saturating_sub(published_at) > u64::from(call_terms.max_policy_rate_age_seconds) {
        return Err(VaultRouterError::PolicyRateTooOld.into());
    }
    let call_start_day = first_full_day(now).map_err(date_step_error)?;
    let series_id = SeriesId::for_pair(timestamp_to_date_key(now), currency, reference_currency)
        .map_err(|_| VaultRouterError::InvalidAssetCurrency)?;
    Ok(LiquidityReservation {
        asset,
        amount_minor: amount,
        gratis_minor,
        snapshot_id: snapshot.to_u256(),
        entry_price_minor,
        policy_rate,
        issuance_currency: currency,
        asset_decimals: decimals,
        reference_currency,
        series_id,
        sub_series: CredisContract::sub_series_of(now),
        call_start_day,
        call_rate: call_terms.call_rate,
        call_window_seconds: call_terms.call_window_seconds,
        call_threshold_seconds: call_terms.call_threshold_seconds,
        call_notice_period_seconds: call_terms.call_notice_period_seconds,
        ..Default::default()
    })
}
