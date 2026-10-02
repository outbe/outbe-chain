//! Quote once, before withdrawing the reserved principal. Issuance never refreshes these terms.
use crate::{
    schema::LiquidityReservation,
    sol_ext::{IReferenceCurrency, IERC20},
};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_credis::constants::{BP_DEN, POLICY_RATE_FACTOR_BP};
use outbe_oracle::{
    api::{self, current_vwap_snapshot, get_finalized_window_vwap},
    schema::OracleContract,
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    math::scaled_math::checked_quote,
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key},
};
fn invalid(message: &str) -> PrecompileError {
    PrecompileError::Revert(message.into())
}
pub(crate) fn quote(
    storage: &StorageHandle<'_>,
    asset: Address,
    amount: U256,
    reference_currency: u16,
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
    api::check_reference_currency_with_storage(storage.clone(), reference_currency)?;
    let snapshot = current_vwap_snapshot(storage.clone())?;
    let valuation = get_finalized_window_vwap(storage.clone(), currency, snapshot)?
        .filter(|v| !v.is_zero())
        .ok_or_else(|| invalid("pledge price unavailable"))?;
    let (collateral, entry_price) = checked_quote(amount, decimals, valuation)?;
    let policy_rate = api::get_policy_rate(storage.clone(), currency)?
        .checked_mul(U256::from(POLICY_RATE_FACTOR_BP))
        .ok_or_else(|| invalid("policy rate overflow"))?
        / U256::from(BP_DEN);
    let now = u64::try_from(storage.timestamp()?).map_err(|_| invalid("timestamp exceeds u64"))?;
    let day = previous_date_key(timestamp_to_date_key(now));
    if OracleContract::new(storage.clone())
        .utc_day_vwap_last_finalized
        .read()?
        < day
    {
        return Err(invalid("previous day VWAP unavailable"));
    }
    let index = api::coen_pair_index_opt(storage.clone(), reference_currency)?
        .ok_or_else(|| invalid("reference pair unavailable"))?;
    let anchor = api::get_utc_day_vwap(storage.clone(), day, index)?
        .filter(|p| !p.is_zero())
        .ok_or_else(|| invalid("previous day VWAP unavailable"))?;
    Ok(LiquidityReservation {
        asset,
        amount,
        collateral,
        snapshot_id: snapshot.to_u256(),
        entry_price,
        valuation_price: valuation,
        policy_rate,
        issuance_currency: currency,
        asset_decimals: decimals,
        reference_currency,
        call_anchor_price: anchor,
        ..Default::default()
    })
}
