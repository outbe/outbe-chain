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
    addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS},
    error::Result,
    storage::StorageHandle,
    units::checked_protocol_to_native,
};
use outbe_vaultrouter::LiquidityReservation;

/// The caller's live reservation. Returns it with the block time.
fn live_reservation(
    storage: &StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
) -> Result<(LiquidityReservation, u64)> {
    let r = outbe_vaultrouter::api::reservation_of(storage, reservation_id)?;
    let now =
        u64::try_from(storage.timestamp()?).map_err(|_| CredisFactoryError::TimestampOverflow)?;
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
                principal_minor: r.amount,
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
            r.amount,
        )?;
        if paid != r.amount || credis.get_credis(id)? != opened {
            return Err(CredisFactoryError::CredisChangedDuringIssuance.into());
        }
        storage.emit_event(
            CREDIS_FACTORY_ADDRESS,
            alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisIssued {
                credisId: id,
                owner: r.credis_owner,
                cca: caller,
                asset: r.asset,
                principalMinor: r.amount,
                gratisMinor: r.gratis_minor,
                callPriceMinor: opened.call_price_minor,
            }),
        )?;
        Ok((id, r.amount))
    })
}

/// Pull `amount` of `asset` from `payer` and deposit it into the VaultRouter.
fn collect_payment(
    storage: &StorageHandle<'_>,
    asset: Address,
    payer: Address,
    amount: U256,
) -> Result<()> {
    let returned = storage.call(
        asset,
        U256::ZERO,
        IERC20::transferFromCall {
            from: payer,
            to: CREDIS_FACTORY_ADDRESS,
            amount,
        }
        .abi_encode()
        .into(),
    )?;
    if !returned.is_empty()
        && IERC20::transferFromCall::abi_decode_returns_validate(&returned) != Ok(true)
    {
        return Err(CredisFactoryError::Erc20TransferFailed.into());
    }
    let returned = storage.call(
        asset,
        U256::ZERO,
        IERC20::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount,
        }
        .abi_encode()
        .into(),
    )?;
    if !returned.is_empty()
        && IERC20::approveCall::abi_decode_returns_validate(&returned) != Ok(true)
    {
        return Err(CredisFactoryError::Erc20ApprovalFailed.into());
    }
    outbe_vaultrouter::api::deposit(storage, asset, amount)?;
    Ok(())
}

/// What paying `credis_id` off now takes: outstanding principal plus accrued interest.
/// Zero once it is Settled or Forfeited.
pub fn quote_settlement(storage: &StorageHandle<'_>, credis_id: U256) -> Result<U256> {
    let record = CredisContract::new(storage.clone()).get_credis(credis_id)?;
    let now =
        u64::try_from(storage.timestamp()?).map_err(|_| CredisFactoryError::TimestampOverflow)?;
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
        let now = u64::try_from(storage.timestamp()?)
            .map_err(|_| CredisFactoryError::TimestampOverflow)?;
        let settlement = rules::settle(&mut credis, credis_id, amount, now)?;
        let after = credis.get_credis(credis_id)?;
        let released = before
            .outstanding_gratis_minor
            .checked_sub(after.outstanding_gratis_minor)
            .ok_or(CredisFactoryError::CollateralIncreased)?;
        if released != settlement.gratis_returned_minor {
            return Err(CredisFactoryError::CollateralReleaseMismatch.into());
        }
        let paid = settlement.total_paid;
        if !paid.is_zero() {
            collect_payment(&storage, settlement.asset, caller, paid)?;
        }
        if credis.get_credis(credis_id)? != after {
            return Err(CredisFactoryError::CredisChangedDuringPayment.into());
        }
        if !released.is_zero() {
            pledges::return_from_credis(&storage, credis_id, released)?;
        }
        Ok((settlement.principal_paid, settlement.interest))
    })
}

/// Forfeit a Credis whose call lapsed.
pub fn forfeit_credis(storage: StorageHandle<'_>, credis_id: U256) -> Result<()> {
    let now =
        u64::try_from(storage.timestamp()?).map_err(|_| CredisFactoryError::TimestampOverflow)?;
    burn_forfeited_collateral(storage, credis_id, |credis| {
        rules::forfeit(credis, credis_id, now)
    })?;
    Ok(())
}

/// The owner forfeits an Issued or Called Credis.
pub fn forfeit_by_owner(
    storage: StorageHandle<'_>,
    caller: Address,
    credis_id: U256,
) -> Result<Forfeit> {
    burn_forfeited_collateral(storage, credis_id, |credis| {
        rules::forfeit_by_owner(credis, credis_id, caller)
    })
}

/// Burn only this Credis's remaining collateral from the source's pledged
/// balance. Fidelity cohorts stay untouched.
fn burn_forfeited_collateral(
    storage: StorageHandle<'_>,
    credis_id: U256,
    forfeit: impl FnOnce(&mut CredisContract<'_>) -> Result<Forfeit>,
) -> Result<Forfeit> {
    storage.with_checkpoint(|| {
        let mut credis = CredisContract::new(storage.clone());
        let before = credis.get_credis(credis_id)?;
        let forfeit = forfeit(&mut credis)?;
        let after = credis.get_credis(credis_id)?;
        let only_state_moved = outbe_credis::Credis {
            state: outbe_credis::CredisState::Forfeited as u8,
            ..before.clone()
        } == after;
        if forfeit.gratis_burned_minor != before.outstanding_gratis_minor || !only_state_moved {
            return Err(CredisFactoryError::ForfeitureCollateralMismatch.into());
        }
        if !forfeit.gratis_burned_minor.is_zero() {
            pledges::burn_from_credis(&storage, credis_id, forfeit.gratis_burned_minor)?;
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .add_to_total_unallocated(forfeit.gratis_burned_minor)?;
        }
        Ok(forfeit)
    })
}
