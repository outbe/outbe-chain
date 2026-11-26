//! Atomic issue, settlement and forfeit of a Credis.
use crate::{
    errors::CredisFactoryError,
    precompile::ICredisFactory,
    sol_ext::{IReferenceCurrency, IERC20},
};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_credis::runtime::effective_state;
use outbe_credis::{CredisContract, CredisState};

use crate::rules::{self, Forfeit, IssueCredisParams};
use outbe_credis::precompile::ICredis;
use outbe_gratis::api as gratis;
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::addresses::{CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS};
use outbe_vaultrouter::CredisReservation;

/// The caller's live reservation. Returns it with the block time.
fn live_reservation(
    storage: &StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
) -> Result<(CredisReservation, u64)> {
    let r = outbe_vaultrouter::api::credis_reservation_of(storage, reservation_id)?;
    let now = storage.now_secs()?;
    let error = if r.asset.is_zero() {
        CredisFactoryError::ReservationNotFound
    } else if r.credis_owner != caller {
        CredisFactoryError::ReservationOwnerMismatch
    } else if now > r.expires_at {
        CredisFactoryError::ReservationExpired
    } else {
        return Ok((r, now));
    };
    Err(error.into())
}

/// Reject an asset whose decimals or currency changed after the reservation quoted it.
fn ensure_asset_metadata(storage: &StorageHandle<'_>, r: &CredisReservation) -> Result<()> {
    let decimals = storage.staticcall(r.asset, IERC20::decimalsCall {}.abi_encode().into())?;
    let decimals = IERC20::decimalsCall::abi_decode_returns_validate(&decimals)
        .map_err(|_| CredisFactoryError::AssetDecimalsUndecodable)?;
    let currency = storage.staticcall(
        r.asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    let currency = IReferenceCurrency::isoCodeCall::abi_decode_returns_validate(&currency)
        .map_err(|_| CredisFactoryError::AssetCurrencyUndecodable)?;
    if currency != r.issuance_currency || decimals != r.asset_decimals {
        return Err(CredisFactoryError::AssetMetadataChanged.into());
    }
    Ok(())
}

/// The cash account issues the Credis of its reservation. The Gratis is pledged from the
/// main account that the sealed `pledge` names. Every term, the call terms included, comes
/// from the reservation.
pub fn issue_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
    pledge: &[u8],
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| {
        let (r, now) = live_reservation(&storage, caller, reservation_id)?;
        outbe_ccaregistry::api::require_active_cca(&storage, r.cca)?;
        ensure_asset_metadata(&storage, &r)?;
        let pledge_account =
            gratis::pledge_sealed(&storage, pledge, reservation_id, r.gratis_minor)?;
        let mut credis = CredisContract::new(storage.clone());
        let (id, newly_opened) = rules::issue_in_series(
            &mut credis,
            IssueCredisParams {
                owner: r.credis_owner,
                cca: r.cca,
                pledge_account,
                asset: r.asset,
                asset_decimals: r.asset_decimals,
                issuance_currency: r.issuance_currency,
                reference_currency: r.reference_currency,
                policy_rate: r.policy_rate,
                amount_minor: r.amount_minor,
                entry_price_minor: r.entry_price_minor,
                gratis_minor: r.gratis_minor,
                issued_at: now,
                series_id: r.series_id,
                sub_series: r.sub_series,
                call_start_day: r.call_start_day,
                call_rate: r.call_rate,
                call_window_seconds: r.call_window_seconds,
                call_threshold_seconds: r.call_threshold_seconds,
                call_notice_period_seconds: r.call_notice_period_seconds,
            },
        )?;
        let opened = credis.get_credis(id)?;
        let paid = outbe_vaultrouter::api::redeem_credis_reservation(&storage, reservation_id, id)?;
        if paid != r.amount_minor || credis.get_credis(id)? != opened {
            return Err(CredisFactoryError::CredisChangedDuringIssuance.into());
        }
        if newly_opened {
            storage.emit_event(
                CREDIS_FACTORY_ADDRESS,
                alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisSeriesOpened {
                    seriesId: opened.series_id.into(),
                    subSeries: opened.sub_series,
                    issuanceCurrency: opened.issuance_currency,
                    referenceCurrency: opened.reference_currency,
                    entryPriceMinor: opened.entry_price_minor,
                    callPriceMinor: opened.call_price_minor,
                    callWindow: u64::from(opened.terms.call_window_seconds),
                    callThreshold: u64::from(opened.terms.call_threshold_seconds),
                    callNoticePeriod: u64::from(opened.terms.call_notice_period_seconds),
                }),
            )?;
        }
        storage.emit_event(
            CREDIS_ADDRESS,
            alloy_sol_types::SolEvent::encode_log_data(&ICredis::CredisIssued {
                credisId: id,
                owner: r.credis_owner,
                seriesId: opened.series_id.into(),
                issuanceCurrency: opened.issuance_currency,
                referenceCurrency: opened.reference_currency,
                cca: r.cca,
                asset: r.asset,
                amountMinor: r.amount_minor,
                gratisMinor: r.gratis_minor,
                policyRate: opened.policy_rate,
            }),
        )?;
        Ok((id, r.amount_minor))
    })
}

/// What paying `credis_id` off now takes: outstanding amount plus accrued fee.
/// Zero once it is Forfeited by its deadline. A deleted Credis has no record to quote.
pub fn quote_settlement(storage: &StorageHandle<'_>, credis_id: U256) -> Result<U256> {
    let record = CredisContract::new(storage.clone()).get_credis(credis_id)?;
    let now = storage.now_secs()?;
    match effective_state(&record, now)? {
        CredisState::Settled | CredisState::Forfeited => Ok(U256::ZERO),
        CredisState::Issued | CredisState::Called => record
            .outstanding_amount_minor
            .checked_add(CredisContract::accrued_fee(&record, now)?)
            .ok_or_else(|| CredisFactoryError::PayoffOverflow.into()),
    }
}

/// Collect payment first, then free the settled share of the Gratis in the main account.
/// A zero `asset` pays in the asset of the Credis.
pub fn settle_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    credis_id: U256,
    asset: Address,
    amount: U256,
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| {
        if amount.is_zero() {
            return Err(CredisFactoryError::InvalidAmount.into());
        }
        let mut credis = CredisContract::new(storage.clone());
        let before = credis.get_credis(credis_id)?;
        let paid_asset = if asset.is_zero() { before.asset } else { asset };
        let now = storage.now_secs()?;
        let repayment = rules::settle(&mut credis, credis_id, amount, now, paid_asset)?;
        if repayment.closed && rules::in_issue_block(&credis, &before)? {
            return Err(CredisFactoryError::CannotFullySettleInIssueBlock.into());
        }
        let after = live_record(&credis, credis_id)?;
        let released = released_gratis(&before, after.as_ref(), &repayment)?;
        let payment = payment_in(&storage, &before, paid_asset, repayment.amount_minor)?;
        outbe_vaultrouter::settlement::collect(&storage, caller, paid_asset, payment)?;
        if live_record(&credis, credis_id)? != after {
            return Err(CredisFactoryError::CredisChangedDuringPayment.into());
        }
        if !released.is_zero() {
            gratis::release_pledged(&storage, before.pledge_account, released)?;
        }
        Ok((repayment.settled_amount_minor, repayment.settled_fee_minor))
    })
}

/// The record of `credis_id`, or `None` once the full settlement deleted it.
fn live_record(
    credis: &CredisContract<'_>,
    credis_id: U256,
) -> Result<Option<outbe_credis::CredisView>> {
    if credis.credis_exists(credis_id)? {
        return Ok(Some(credis.get_credis(credis_id)?));
    }
    Ok(None)
}

/// The Gratis the settlement freed, checked against the repayment.
fn released_gratis(
    before: &outbe_credis::CredisView,
    after: Option<&outbe_credis::CredisView>,
    repayment: &rules::Repayment,
) -> Result<U256> {
    let outstanding = after.map_or(U256::ZERO, |record| record.outstanding_gratis_minor);
    let released = before
        .outstanding_gratis_minor
        .checked_sub(outstanding)
        .ok_or(CredisFactoryError::CollateralIncreased)?;
    if released != repayment.unpledged_gratis_minor {
        return Err(CredisFactoryError::CollateralReleaseMismatch.into());
    }
    Ok(released)
}

/// The payment of `amount_minor` (Credis asset units) in `paid_asset`. Another asset is
/// allowed only while the Credis asset has no vault, and it pays at par in its own units.
fn payment_in(
    storage: &StorageHandle<'_>,
    record: &outbe_credis::Credis,
    paid_asset: Address,
    amount_minor: U256,
) -> Result<U256> {
    if paid_asset == record.asset {
        return Ok(amount_minor);
    }
    let router = outbe_vaultrouter::VaultRouterContract::new(storage.clone());
    let allowed = router.first_vault(record.asset)?.is_none()
        && router.first_vault(paid_asset)?.is_some()
        && iso_code(storage, paid_asset)? == record.issuance_currency;
    if !allowed {
        return Err(CredisFactoryError::SettlementAssetNotAllowed.into());
    }
    outbe_core_common::settlement::floor_to_asset_units(
        amount_minor,
        U256::ONE,
        record.asset_decimals,
        decimals(storage, paid_asset)?,
    )
    .map_err(|_| CredisFactoryError::SettlementAssetNotAllowed.into())
}

fn decimals(storage: &StorageHandle<'_>, asset: Address) -> Result<u8> {
    let out = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns_validate(&out)
        .map_err(|_| CredisFactoryError::AssetDecimalsUndecodable.into())
}

fn iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let out = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns_validate(&out)
        .map_err(|_| CredisFactoryError::AssetCurrencyUndecodable.into())
}

/// Deletes a Credis of a forfeited group.
pub(crate) fn delete_forfeited_credis(storage: StorageHandle<'_>, credis_id: U256) -> Result<()> {
    burn_forfeited_collateral(storage, credis_id, |_, _| Ok(()))?;
    Ok(())
}

/// The owner forfeits an Issued or Called Credis.
pub fn forfeit_by_owner(
    storage: StorageHandle<'_>,
    caller: Address,
    credis_id: U256,
) -> Result<Forfeit> {
    burn_forfeited_collateral(storage, credis_id, |credis, record| {
        rules::validate_forfeit_by_owner(credis, record, caller)
    })
}

/// Burns only this Credis's remaining collateral from the pledged balance of its main account
/// and returns it to the Promis Reserve, together.
fn burn_forfeited_collateral(
    storage: StorageHandle<'_>,
    credis_id: U256,
    validate: impl FnOnce(&CredisContract<'_>, &outbe_credis::CredisView) -> Result<()>,
) -> Result<Forfeit> {
    storage.with_checkpoint(|| {
        let mut credis = CredisContract::new(storage.clone());
        let before = credis.get_credis(credis_id)?;
        validate(&credis, &before)?;
        if before.outstanding_amount_minor.is_zero() {
            return Err(CredisFactoryError::NothingOutstanding.into());
        }
        let pledged = before.outstanding_gratis_minor;
        if !pledged.is_zero() {
            gratis::burn_pledged(&storage, before.pledge_account, pledged)?;
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .add(outbe_promislimit::LimitSource::Credis, pledged)?;
        }
        let forfeit = rules::delete_forfeited(&mut credis, before.record)?;
        if forfeit.forfeited_gratis_minor != pledged || credis.credis_exists(credis_id)? {
            return Err(CredisFactoryError::ForfeitureCollateralMismatch.into());
        }
        Ok(forfeit)
    })
}
