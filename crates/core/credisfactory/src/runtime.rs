//! Atomic reservation, pledge and collateral transitions.
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
use outbe_gratisfactory::api as pledges;
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS, error::Result, storage::StorageHandle,
    units::checked_protocol_to_native,
};
use outbe_vaultrouter::LiquidityReservation;

fn maintenance_context<'s>(
    storage: &StorageHandle<'s>,
) -> Result<outbe_primitives::block::BlockRuntimeContext<'s>> {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    Ok(BlockRuntimeContext::new(
        BlockContext {
            block_number: storage.block_number()?,
            timestamp: storage.now_secs()?,
            chain_id: storage.chain_id()?,
            genesis_hash: storage.genesis_hash()?,
            genesis_timestamp: storage.genesis_timestamp()?,
            ..Default::default()
        },
        storage.clone(),
    ))
}

/// Run selected phases in pinned-day order with the shared per-height allowance.
pub fn maintain(
    storage: &StorageHandle<'_>,
    selection: outbe_oracle::call_pass::Selection,
) -> Result<()> {
    outbe_primitives::dispatch::ensure_mutation_allowed(storage)?;
    // TODO: Reward callers for maintenance work performed.
    if selection == outbe_oracle::call_pass::Selection::default() {
        return Ok(());
    }
    crate::called::process_call_pass(&maintenance_context(storage)?, selection)?;
    Ok(())
}

/// Read pending daily obligations without scheduling or changing economic state.
pub fn has_maintenance_work(storage: &StorageHandle<'_>) -> Result<(bool, bool, bool)> {
    crate::called::pending_work(&maintenance_context(storage)?)
}

/// The caller's live reservation. Returns it with the block time.
fn live_reservation(
    storage: &StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
) -> Result<(LiquidityReservation, u64)> {
    let r = outbe_vaultrouter::api::reservation_of(storage, reservation_id)?;
    let now = storage.now_secs()?;
    let error = if r.asset.is_zero() {
        CredisFactoryError::ReservationNotFound
    } else if r.cca != caller {
        CredisFactoryError::ReservationCcaMismatch
    } else if now > r.expires_at {
        CredisFactoryError::ReservationExpired
    } else {
        return Ok((r, now));
    };
    Err(error.into())
}

/// Reject an asset whose decimals or currency changed after the reservation quoted it.
fn ensure_asset_metadata(storage: &StorageHandle<'_>, r: &LiquidityReservation) -> Result<()> {
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

/// Issues a Credis on the reservation's pledge. Every term, the call terms included,
/// comes from the reservation.
pub fn issue_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
    stake: U256,
) -> Result<(U256, U256)> {
    storage.with_checkpoint(|| {
        outbe_ccaregistry::api::require_active_cca(&storage, caller)?;
        let (r, now) = live_reservation(&storage, caller, reservation_id)?;
        ensure_asset_metadata(&storage, &r)?;
        let required = checked_protocol_to_native(r.gratis_minor)
            .ok_or(CredisFactoryError::CoenStakeOverflow)?;
        if stake != required {
            return Err(CredisFactoryError::CcaStakeMismatch.into());
        }
        let mut credis = CredisContract::new(storage.clone());
        let id = rules::issue(
            &mut credis,
            IssueCredisParams {
                owner: r.credis_owner,
                cca: caller,
                source: r.source,
                asset: r.asset,
                issuance_currency: r.issuance_currency,
                reference_currency: r.reference_currency,
                policy_rate: r.policy_rate,
                principal_minor: r.amount_minor,
                entry_price_minor: r.entry_price_minor,
                call_anchor_price_minor: r.call_anchor_price_minor,
                gratis_minor: r.gratis_minor,
                issued_at: now,
            },
        )?;
        pledges::send_to_credis(&storage, reservation_id, id, r.source, r.gratis_minor)?;
        let opened = credis.get_credis(id)?;
        storage.transfer_balance(CREDIS_FACTORY_ADDRESS, r.credis_owner, stake)?;
        let paid = outbe_vaultrouter::api::release_reservation(
            &storage,
            reservation_id,
            r.credis_owner,
            r.amount_minor,
        )?;
        if paid != r.amount_minor || credis.get_credis(id)? != opened {
            return Err(CredisFactoryError::CredisChangedDuringIssuance.into());
        }
        storage.emit_event(
            CREDIS_FACTORY_ADDRESS,
            alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisIssued {
                credisId: id,
                owner: r.credis_owner,
                cca: caller,
                asset: r.asset,
                principalMinor: r.amount_minor,
                gratisMinor: r.gratis_minor,
                callPriceMinor: opened.call_price_minor,
            }),
        )?;
        Ok((id, r.amount_minor))
    })
}

/// What paying `credis_id` off now takes: outstanding principal plus accrued interest.
/// Zero once it is Settled or Forfeited.
pub fn quote_settlement(storage: &StorageHandle<'_>, credis_id: U256) -> Result<U256> {
    let record = CredisContract::new(storage.clone()).get_credis(credis_id)?;
    let now = storage.now_secs()?;
    match effective_state(&record, now)? {
        CredisState::Settled | CredisState::Forfeited => Ok(U256::ZERO),
        CredisState::Issued | CredisState::Called => record
            .outstanding_principal_minor
            .checked_add(CredisContract::accrued_interest(&record, now)?)
            .ok_or_else(|| CredisFactoryError::PayoffOverflow.into()),
    }
}

/// Collect payment first, then return the released collateral to the source's
/// liquid balance. No Fidelity mutation.
pub fn settle_credis(
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
        let now = storage.now_secs()?;
        let repayment = rules::settle(&mut credis, credis_id, amount, now)?;
        let after = credis.get_credis(credis_id)?;
        let released = before
            .outstanding_gratis_minor
            .checked_sub(after.outstanding_gratis_minor)
            .ok_or(CredisFactoryError::CollateralIncreased)?;
        if released != repayment.gratis_returned_minor {
            return Err(CredisFactoryError::CollateralReleaseMismatch.into());
        }
        outbe_vaultrouter::settlement::collect(
            &storage,
            CREDIS_FACTORY_ADDRESS,
            caller,
            repayment.asset,
            repayment.total_paid,
        )?;
        if credis.get_credis(credis_id)? != after {
            return Err(CredisFactoryError::CredisChangedDuringPayment.into());
        }
        if !released.is_zero() {
            pledges::return_from_credis(&storage, credis_id, released)?;
        }
        Ok((repayment.principal_paid, repayment.interest))
    })
}

/// Deletes a Credis of a forfeited group.
pub(crate) fn delete_forfeited_credis(storage: StorageHandle<'_>, credis_id: U256) -> Result<()> {
    burn_forfeited_collateral(storage, credis_id, |credis, record| {
        rules::delete_forfeited(credis, record)
    })?;
    Ok(())
}

/// The owner forfeits an Issued or Called Credis.
pub fn forfeit_by_owner(
    storage: StorageHandle<'_>,
    caller: Address,
    credis_id: U256,
) -> Result<Forfeit> {
    burn_forfeited_collateral(storage, credis_id, |credis, _| {
        rules::forfeit_by_owner(credis, credis_id, caller)
    })
}

/// Burns only this Credis's remaining collateral from the source's pledged balance and
/// returns it to the Promis Reserve, together. Fidelity cohorts stay untouched.
fn burn_forfeited_collateral(
    storage: StorageHandle<'_>,
    credis_id: U256,
    forfeit: impl FnOnce(&mut CredisContract<'_>, outbe_credis::Credis) -> Result<Forfeit>,
) -> Result<Forfeit> {
    storage.with_checkpoint(|| {
        let mut credis = CredisContract::new(storage.clone());
        let before = credis.get_credis(credis_id)?;
        let pledged = before.outstanding_gratis_minor;
        let forfeit = forfeit(&mut credis, before)?;
        if forfeit.gratis_burned_minor != pledged || credis.credis_exists(credis_id)? {
            return Err(CredisFactoryError::ForfeitureCollateralMismatch.into());
        }
        if !forfeit.gratis_burned_minor.is_zero() {
            pledges::burn_from_credis(&storage, credis_id, forfeit.gratis_burned_minor)?;
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .checked_credit(forfeit.gratis_burned_minor)?;
        }
        Ok(forfeit)
    })
}
