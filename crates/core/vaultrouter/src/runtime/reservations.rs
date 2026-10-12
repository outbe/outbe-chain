//! CCA liquidity reservations held in the router's custody for a smart account.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use super::calls::{erc20_transfer, vault_deposit, vault_withdraw};
use super::{ensure_shares_cover, first_vault, SELF};
use crate::api::IVaultRouter;
use crate::constants::RESERVATION_TTL_SECS;
use crate::errors::VaultRouterError;
use crate::schema::{LiquidityReservation, VaultRouterContract};

/// `reserveStables`: redeem `amount` of `asset` from its origin vault into this
/// router's custody for `smart_account`. Caller must be an active CCA.
pub(crate) fn reserve_stables(
    storage: StorageHandle<'_>,
    caller: Address,
    call: IVaultRouter::reserveStablesCall,
) -> Result<U256> {
    let IVaultRouter::reserveStablesCall {
        smartAccount: smart_account,
        source,
        asset,
        amount,
    } = call;
    storage.with_checkpoint(|| {
        outbe_ccaregistry::api::require_active_cca(&storage, caller)?;
        if smart_account.is_zero() || source.is_zero() || asset.is_zero() {
            return Err(VaultRouterError::ZeroAddress.into());
        }
        if amount.is_zero() {
            return Err(VaultRouterError::InvalidReservationAmount.into());
        }

        let now = now_secs(&storage)?;
        let expires_at = now
            .checked_add(RESERVATION_TTL_SECS)
            .ok_or(VaultRouterError::TimestampOverflow)?;
        let vault = first_vault(&storage, asset)?;
        ensure_shares_cover(&storage, vault, amount)?;
        let mut terms = crate::reservation::quote(&storage, asset, amount)?;

        let contract = VaultRouterContract::new(storage.clone());
        let nonce = contract
            .reservation_nonce
            .read()?
            .checked_add(U256::from(1))
            .ok_or(VaultRouterError::InvalidReservationAmount)?;
        contract.reservation_nonce.write(nonce)?;
        let id = nonce;
        if contract.reservations.exists(id)? {
            return Err(VaultRouterError::ReservationExists(id).into());
        }

        vault_withdraw(&storage, vault, amount, SELF, SELF)?;
        terms.id = id;
        terms.smart_account = smart_account;
        terms.cca = caller;
        terms.source = source;
        terms.vault = vault;
        terms.expires_at = expires_at;
        contract.reservations.create(&terms)?;

        let mut contract = VaultRouterContract::new(storage.clone());
        contract.emit(IVaultRouter::ReservationCreated {
            id,
            smartAccount: smart_account,
            cca: caller,
            asset,
            vault,
            amount,
            expiresAt: expires_at,
        })?;
        Ok(id)
    })
}

/// Validate the reserved account, pay its recorded CCA for COEN delivered to the user,
/// and return any unused remainder to the origin vault.
pub(crate) fn release_reservation(
    storage: StorageHandle<'_>,
    id: U256,
    receiver: Address,
    amount: U256,
    target: IVaultRouter::StablesTarget,
) -> Result<U256> {
    let error = if receiver.is_zero() {
        VaultRouterError::ZeroAddress
    } else if matches!(target, IVaultRouter::StablesTarget::Unknown) {
        VaultRouterError::InvalidLiquidityTarget
    } else if amount.is_zero() {
        VaultRouterError::InvalidReservationAmount
    } else {
        return release_checked(storage, id, receiver, amount);
    };
    Err(error.into())
}

fn release_checked(
    storage: StorageHandle<'_>,
    id: U256,
    receiver: Address,
    amount: U256,
) -> Result<U256> {
    let now = now_secs(&storage)?;
    storage.with_checkpoint(|| {
        let record = take_reservation(&storage, id)?;
        ensure_releasable(&record, id, now, receiver, amount)?;

        erc20_transfer(&storage, record.asset, record.cca, amount)?;

        let excess = record.amount - amount;
        let mut returned_shares = U256::ZERO;
        if !excess.is_zero() {
            returned_shares = vault_deposit(&storage, record.vault, excess, SELF)?;
        }

        let mut contract = VaultRouterContract::new(storage.clone());
        contract.emit(IVaultRouter::ReservationReleased {
            id,
            asset: record.asset,
            receiver: record.cca,
            amount,
        })?;
        if !excess.is_zero() {
            contract.emit(IVaultRouter::ReservationReturned {
                id,
                asset: record.asset,
                vault: record.vault,
                amount: excess,
                mintedShares: returned_shares,
            })?;
        }
        Ok(amount)
    })
}

/// Rejects a release that the reservation's expiry, account or amount does not cover.
fn ensure_releasable(
    record: &LiquidityReservation,
    id: U256,
    now: u64,
    receiver: Address,
    amount: U256,
) -> Result<()> {
    let error = if now > record.expires_at {
        VaultRouterError::ReservationExpired(id)
    } else if receiver != record.smart_account {
        VaultRouterError::ReservationAccountMismatch
    } else if amount > record.amount {
        VaultRouterError::ReservationInsufficient {
            available: record.amount,
            required: amount,
        }
    } else {
        return Ok(());
    };
    Err(error.into())
}

/// `returnReservation`: deposit the assets held under `id` back into their origin
/// vault. The originating CCA may unwind anytime. After expiry, anyone may.
/// Idempotent: an unknown id returns zero.
pub(crate) fn return_reservation(
    storage: StorageHandle<'_>,
    caller: Address,
    id: U256,
) -> Result<U256> {
    let now = now_secs(&storage)?;
    storage.with_checkpoint(|| {
        let Some(record) = take_reservation_if_held(&storage, id)? else {
            return Ok(U256::ZERO);
        };
        if caller != record.cca && now <= record.expires_at {
            return Err(VaultRouterError::Unauthorized.into());
        }

        let minted_shares = vault_deposit(&storage, record.vault, record.amount, SELF)?;
        let mut contract = VaultRouterContract::new(storage.clone());
        contract.emit(IVaultRouter::ReservationReturned {
            id,
            asset: record.asset,
            vault: record.vault,
            amount: record.amount,
            mintedShares: minted_shares,
        })?;
        Ok(minted_shares)
    })
}

/// Reads and deletes the reservation under `id`, rejecting an unknown one.
fn take_reservation(storage: &StorageHandle<'_>, id: U256) -> Result<LiquidityReservation> {
    take_reservation_if_held(storage, id)?
        .ok_or_else(|| VaultRouterError::ReservationNotFound(id).into())
}

/// Reads and deletes the reservation under `id`, or `None` when nothing is held.
fn take_reservation_if_held(
    storage: &StorageHandle<'_>,
    id: U256,
) -> Result<Option<LiquidityReservation>> {
    let contract = VaultRouterContract::new(storage.clone());
    let Some(record) = contract.reservations.get(id)? else {
        return Ok(None);
    };
    contract.reservations.delete(id)?;
    Ok(Some(record))
}

fn now_secs(storage: &StorageHandle<'_>) -> Result<u64> {
    storage
        .timestamp()?
        .try_into()
        .map_err(|_| VaultRouterError::TimestampOverflow.into())
}

/// `reservationOf`: the reservation held under `id`, or a zeroed record when none.
pub fn reservation_of(storage: &StorageHandle<'_>, id: U256) -> Result<LiquidityReservation> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(contract
        .reservations
        .get(id)?
        .unwrap_or(LiquidityReservation {
            id,
            ..Default::default()
        }))
}
