//! Atomic reservation, pledge and collateral transitions.
use crate::{
    errors::CredisFactoryError,
    precompile::ICredisFactory,
    sol_ext::{IReferenceCurrency, IERC20},
};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_credis::constants::{BP_DEN, POLICY_RATE_FACTOR_BP};
use outbe_credis::{CredisContract, IssueCredisParams};
use outbe_gratisfactory::api as pledges;
use outbe_oracle::api::{
    check_reference_currency_with_storage, coen_pair_index_opt, get_policy_rate, get_utc_day_vwap,
};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS},
    error::{PrecompileError, Result},
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key},
    units::checked_protocol_to_native,
};
use outbe_vaultrouter::LiquidityReservation;

fn revert(message: &str) -> PrecompileError {
    PrecompileError::Revert(message.into())
}

/// The caller's live reservation, with a deployed smart account. Returns it with the block time.
fn live_reservation(
    storage: &StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
) -> Result<(LiquidityReservation, u64)> {
    let r = outbe_vaultrouter::api::reservation_of(storage, reservation_id)?;
    let now = u64::try_from(storage.timestamp()?).map_err(|_| revert("timestamp exceeds u64"))?;
    let error = if r.asset.is_zero() {
        CredisFactoryError::ReservationNotFound
    } else if r.cca != caller {
        CredisFactoryError::ReservationCcaMismatch
    } else if now > r.expires_at {
        CredisFactoryError::ReservationExpired
    } else if r.smart_account.is_zero()
        || storage.with_account_info(r.smart_account, |info| Ok(info.is_empty_code_hash()))?
    {
        CredisFactoryError::SmartAccountNotDeployed
    } else {
        return Ok((r, now));
    };
    Err(error.into())
}

/// Reject an asset whose decimals or currency changed after the reservation quoted it.
fn ensure_asset_metadata(storage: &StorageHandle<'_>, r: &LiquidityReservation) -> Result<()> {
    let decimals = storage.staticcall(r.asset, IERC20::decimalsCall {}.abi_encode().into())?;
    let decimals = IERC20::decimalsCall::abi_decode_returns_validate(&decimals)
        .map_err(|_| revert("asset decimals undecodable"))?;
    let currency = storage.staticcall(
        r.asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    let currency = IReferenceCurrency::isoCodeCall::abi_decode_returns_validate(&currency)
        .map_err(|_| revert("asset currency undecodable"))?;
    if currency != r.issuance_currency || decimals != r.asset_decimals {
        return Err(revert("asset metadata changed"));
    }
    Ok(())
}

/// The previous closed UTC-day VWAP in `reference_currency` at `now`: the call anchor.
fn call_anchor_at(storage: &StorageHandle<'_>, reference_currency: u16, now: u64) -> Result<U256> {
    let day = previous_date_key(timestamp_to_date_key(now));
    let finalized = OracleContract::new(storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < day {
        return Err(CredisFactoryError::PreviousDayVwapUnavailable.into());
    }
    let Some(index) = coen_pair_index_opt(storage.clone(), reference_currency)? else {
        return Err(CredisFactoryError::PreviousDayVwapUnavailable.into());
    };
    match get_utc_day_vwap(storage.clone(), day, index)? {
        Some(vwap) if !vwap.is_zero() => Ok(vwap),
        _ => Err(CredisFactoryError::PreviousDayVwapUnavailable.into()),
    }
}

/// The issuance currency's policy rate, scaled by the policy-rate factor.
fn policy_rate_of(storage: &StorageHandle<'_>, issuance_currency: u16) -> Result<U256> {
    get_policy_rate(storage.clone(), issuance_currency)?
        .checked_mul(U256::from(POLICY_RATE_FACTOR_BP))
        .map(|v| v / U256::from(BP_DEN))
        .ok_or_else(|| revert("credis policy rate overflow"))
}

/// Issues a Credis on the reservation's pledge. The pledge terms come from the
/// reservation; `reference_currency`, the call anchor and the policy rate are
/// fixed here, at issuance.
pub fn issue_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
    reference_currency: u16,
    stake: U256,
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| {
        outbe_ccaregistry::api::require_active_cca(&storage, caller)?;
        let (r, now) = live_reservation(&storage, caller, reservation_id)?;
        ensure_asset_metadata(&storage, &r)?;
        // The call sweep walks only registered currencies.
        check_reference_currency_with_storage(storage.clone(), reference_currency)?;
        let policy_rate = policy_rate_of(&storage, r.issuance_currency)?;
        let call_anchor_price_minor = call_anchor_at(&storage, reference_currency, now)?;
        let required = checked_protocol_to_native(r.gratis_minor)
            .ok_or_else(|| revert("COEN stake overflow"))?;
        if stake != required {
            return Err(CredisFactoryError::CcaStakeMismatch.into());
        }
        let mut credis = CredisContract::new(storage.clone());
        let id = credis.issue(IssueCredisParams {
            owner: r.smart_account,
            cca: caller,
            source: r.source,
            asset: r.asset,
            issuance_currency: r.issuance_currency,
            reference_currency,
            policy_rate,
            principal_minor: r.amount,
            entry_price_minor: r.entry_price_minor,
            call_anchor_price_minor,
            gratis_minor: r.gratis_minor,
            issued_at: now,
        })?;
        pledges::send_to_credis(&storage, reservation_id, id, r.source, r.gratis_minor)?;
        let opened = credis.get_credis(id)?;
        storage.transfer_balance(CREDIS_FACTORY_ADDRESS, r.smart_account, stake)?;
        let paid = outbe_vaultrouter::api::release_reservation(
            &storage,
            reservation_id,
            r.smart_account,
            r.amount,
        )?;
        if paid != r.amount || credis.get_credis(id)? != opened {
            return Err(revert("Credis changed during issuance"));
        }
        storage.emit_event(
            CREDIS_FACTORY_ADDRESS,
            alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisIssued {
                credisId: id,
                owner: r.smart_account,
                cca: caller,
                asset: r.asset,
                principalMinor: r.amount,
                gratisMinor: r.gratis_minor,
            }),
        )?;
        Ok((id, r.amount))
    })
}

/// Collect payment first, then return the released collateral to the source's
/// liquid balance. No Fidelity mutation.
pub fn settle(
    storage: StorageHandle<'_>,
    caller: Address,
    credis_id: U256,
    amount: U256,
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| {
        if amount.is_zero() {
            return Err(CredisFactoryError::InvalidAmount.into());
        }
        let mut credis = CredisContract::new(storage.clone());
        let before = credis.get_credis(credis_id)?;
        let now =
            u64::try_from(storage.timestamp()?).map_err(|_| revert("timestamp exceeds u64"))?;
        let settlement = credis.settle(credis_id, amount, now)?;
        let after = credis.get_credis(credis_id)?;
        let released = before
            .outstanding_gratis_minor
            .checked_sub(after.outstanding_gratis_minor)
            .ok_or_else(|| revert("collateral increased"))?;
        if released != settlement.gratis_returned_minor {
            return Err(revert("collateral release mismatch"));
        }
        let paid = settlement.total_paid;
        if !paid.is_zero() {
            let returned = storage.call(
                settlement.asset,
                U256::ZERO,
                IERC20::transferFromCall {
                    from: caller,
                    to: CREDIS_FACTORY_ADDRESS,
                    amount: paid,
                }
                .abi_encode()
                .into(),
            )?;
            if !returned.is_empty()
                && IERC20::transferFromCall::abi_decode_returns_validate(&returned) != Ok(true)
            {
                return Err(revert("ERC20 transfer failed"));
            }
            let returned = storage.call(
                settlement.asset,
                U256::ZERO,
                IERC20::approveCall {
                    spender: VAULT_ROUTER_ADDRESS,
                    amount: paid,
                }
                .abi_encode()
                .into(),
            )?;
            if !returned.is_empty()
                && IERC20::approveCall::abi_decode_returns_validate(&returned) != Ok(true)
            {
                return Err(revert("ERC20 approval failed"));
            }
            outbe_vaultrouter::api::deposit(&storage, settlement.asset, paid)?;
        }
        if credis.get_credis(credis_id)? != after {
            return Err(revert("Credis changed during payment"));
        }
        if !released.is_zero() {
            pledges::return_from_credis(&storage, credis_id, released)?;
        }
        Ok((settlement.principal_paid, settlement.interest))
    })
}

/// Burn only this Credis's remaining collateral from the source's pledged
/// balance. Fidelity cohorts stay untouched.
pub fn forfeit_credis(storage: StorageHandle<'_>, credis_id: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        let now =
            u64::try_from(storage.timestamp()?).map_err(|_| revert("timestamp exceeds u64"))?;
        let mut credis = CredisContract::new(storage.clone());
        let before = credis.get_credis(credis_id)?;
        let forfeit = credis.forfeit(credis_id, now)?;
        let after = credis.get_credis(credis_id)?;
        let only_state_moved = outbe_credis::Credis {
            state: outbe_credis::CredisState::Forfeited as u8,
            ..before.clone()
        } == after;
        if forfeit.gratis_burned_minor != before.outstanding_gratis_minor || !only_state_moved {
            return Err(revert("forfeiture collateral mismatch"));
        }
        if !forfeit.gratis_burned_minor.is_zero() {
            pledges::burn_from_credis(&storage, credis_id, forfeit.gratis_burned_minor)?;
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .add_to_total_unallocated(forfeit.gratis_burned_minor)?;
        }
        Ok(())
    })
}
