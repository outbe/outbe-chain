//! Quote the pledge once, before withdrawing the reserved principal. Issuance never
//! refreshes it; the Credis call terms are read at issuance, not here.
use crate::{
    schema::LiquidityReservation,
    sol_ext::{IReferenceCurrency, IERC20},
};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_oracle::api::{self, current_vwap_snapshot, get_finalized_window_vwap};
use outbe_primitives::{
    error::{PrecompileError, Result},
    math::scaled_math::checked_quote,
    storage::StorageHandle,
};
fn invalid(message: &str) -> PrecompileError {
    PrecompileError::Revert(message.into())
}
pub(crate) fn quote(
    storage: &StorageHandle<'_>,
    asset: Address,
    amount: U256,
) -> Result<LiquidityReservation> {
    if storage.with_account_info(asset, |info| Ok(info.is_empty_code_hash()))? {
        return Err(invalid("invalid asset"));
    }
    let data = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    let decimals = IERC20::decimalsCall::abi_decode_returns_validate(&data)
        .map_err(|_| invalid("invalid asset decimals"))?;
    if decimals > 18 {
        return Err(invalid("unsupported asset decimals"));
    }
    let data = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    let currency = IReferenceCurrency::isoCodeCall::abi_decode_returns_validate(&data)
        .map_err(|_| invalid("invalid asset currency"))?;
    api::check_reference_currency_with_storage(storage.clone(), currency)?;
    // Issuance reads the rate. Checking it here only spares a pledge an issuance
    // that would revert.
    api::get_policy_rate(storage.clone(), currency)?;
    let snapshot = current_vwap_snapshot(storage.clone())?;
    let valuation = get_finalized_window_vwap(storage.clone(), currency, snapshot)?
        .filter(|v| !v.is_zero())
        .ok_or_else(|| invalid("pledge price unavailable"))?;
    let (gratis_minor, entry_price_minor) = checked_quote(amount, decimals, valuation)?;
    Ok(LiquidityReservation {
        asset,
        amount,
        gratis_minor,
        snapshot_id: snapshot.to_u256(),
        entry_price_minor,
        valuation_price_minor: valuation,
        issuance_currency: currency,
        asset_decimals: decimals,
        ..Default::default()
    })
}
