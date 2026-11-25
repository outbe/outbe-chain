//! CCA liquidity reservations held in the router's custody for a Credis owner.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_credis::CredisContract;
use outbe_oracle::api::{self, current_vwap_24h_snapshot, vwap_24h};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::first_full_day;
use outbe_system_common::{math::scaled_math::checked_quote, units::MAX_ASSET_DECIMALS};

use super::calls::{erc20_transfer, vault_deposit, vault_withdraw};
use super::{ensure_shares_cover, first_vault, SELF};
use crate::constants::RESERVATION_TTL_SECS;
use crate::errors::{date_step_error, VaultRouterError};
use crate::precompile::IVaultRouter;
use crate::schema::{CredisReservation, VaultRouterContract};
use crate::sol_ext::{IReferenceCurrency, IERC20};

/// `createCredisReservation`: redeem `amount` of `asset` from its origin vault into this
/// router's custody for `credis_owner`. Caller must be an active CCA.
pub(crate) fn create_credis_reservation(
    storage: StorageHandle<'_>,
    caller: Address,
    call: IVaultRouter::createCredisReservationCall,
) -> Result<U256> {
    let IVaultRouter::createCredisReservationCall {
        credisOwner: credis_owner,
        source,
        asset,
        amountMinor: amount,
        referenceCurrency: reference_currency,
    } = call;
    storage.with_checkpoint(|| {
        outbe_ccaregistry::api::require_active_cca(&storage, caller)?;
        if credis_owner.is_zero() || source.is_zero() || asset.is_zero() {
            return Err(VaultRouterError::ZeroAddress.into());
        }
        if amount.is_zero() {
            return Err(VaultRouterError::InvalidReservationAmount.into());
        }

        let now = storage.now_secs()?;
        let expires_at = now
            .checked_add(RESERVATION_TTL_SECS)
            .ok_or(VaultRouterError::TimestampOverflow)?;
        let vault = first_vault(&storage, asset)?;
        let contract = VaultRouterContract::new(storage.clone());
        let proposal_id = contract.pending_vault_proposals.read(&vault)?;
        if !proposal_id.is_zero() {
            let action = crate::state::vault_action(&storage, proposal_id)?;
            if action.vault != vault {
                return Err(VaultRouterError::VaultActionIndexInconsistent(proposal_id).into());
            }
            // Expired removals still block admission until explicit cleanup or supersession.
            if action.kind == crate::state::REMOVE_VAULT {
                return Err(
                    VaultRouterError::ReserveVaultRemovalPending { vault, proposal_id }.into(),
                );
            }
        }
        ensure_shares_cover(&storage, vault, amount)?;
        let mut terms = quote(&storage, asset, amount, reference_currency)?;

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
        terms.credis_owner = credis_owner;
        terms.cca = caller;
        terms.source = source;
        terms.vault = vault;
        terms.expires_at = expires_at;
        contract.reservations.create(&terms)?;

        let mut contract = VaultRouterContract::new(storage.clone());
        contract.emit(IVaultRouter::ReservationCreated {
            id,
            credisOwner: credis_owner,
            cca: caller,
            asset,
            vault,
            amountMinor: amount,
            expiresAt: expires_at,
        })?;
        Ok(id)
    })
}

/// Validate the receiver as the reservation's Credis owner, pay the recorded CCA for COEN delivered to it,
/// and return any unused remainder to the origin vault.
pub(crate) fn redeem_credis_reservation(
    storage: StorageHandle<'_>,
    id: U256,
    receiver: Address,
    amount: U256,
    target: IVaultRouter::StablecoinTarget,
) -> Result<U256> {
    let error = if receiver.is_zero() {
        VaultRouterError::ZeroAddress
    } else if matches!(target, IVaultRouter::StablecoinTarget::Unknown) {
        VaultRouterError::InvalidStablecoinTarget
    } else if amount.is_zero() {
        VaultRouterError::InvalidReservationAmount
    } else {
        return redeem_credis_reservation_checked(storage, id, receiver, amount);
    };
    Err(error.into())
}

fn redeem_credis_reservation_checked(
    storage: StorageHandle<'_>,
    id: U256,
    receiver: Address,
    amount: U256,
) -> Result<U256> {
    let now = storage.now_secs()?;
    storage.with_checkpoint(|| {
        let record = take_reservation(&storage, id)?;
        ensure_releasable(&record, id, now, receiver, amount)?;

        erc20_transfer(&storage, record.asset, record.cca, amount)?;

        let excess = record.amount_minor - amount;
        let mut returned_shares = U256::ZERO;
        if !excess.is_zero() {
            returned_shares = vault_deposit(&storage, record.vault, excess, SELF)?;
        }

        let mut contract = VaultRouterContract::new(storage.clone());
        contract.emit(IVaultRouter::ReservationReleased {
            id,
            asset: record.asset,
            receiver: record.cca,
            amountMinor: amount,
        })?;
        if !excess.is_zero() {
            contract.emit(IVaultRouter::ReservationReturned {
                id,
                asset: record.asset,
                vault: record.vault,
                amountMinor: excess,
                mintedShares: returned_shares,
            })?;
        }
        Ok(amount)
    })
}

/// Rejects a release that the reservation's expiry, Credis owner or amount does not cover.
fn ensure_releasable(
    record: &CredisReservation,
    id: U256,
    now: u64,
    receiver: Address,
    amount: U256,
) -> Result<()> {
    let error = if now > record.expires_at {
        VaultRouterError::ReservationExpired(id)
    } else if receiver != record.credis_owner {
        VaultRouterError::CredisOwnerMismatch
    } else if amount > record.amount_minor {
        VaultRouterError::ReservationInsufficient {
            available: record.amount_minor,
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
    let now = storage.now_secs()?;
    storage.with_checkpoint(|| {
        let Some(record) = take_reservation_if_held(&storage, id)? else {
            return Ok(U256::ZERO);
        };
        if caller != record.cca && now <= record.expires_at {
            return Err(VaultRouterError::Unauthorized.into());
        }

        let minted_shares = vault_deposit(&storage, record.vault, record.amount_minor, SELF)?;
        let mut contract = VaultRouterContract::new(storage.clone());
        contract.emit(IVaultRouter::ReservationReturned {
            id,
            asset: record.asset,
            vault: record.vault,
            amountMinor: record.amount_minor,
            mintedShares: minted_shares,
        })?;
        Ok(minted_shares)
    })
}

/// Reads and deletes the reservation under `id`, rejecting an unknown one.
fn take_reservation(storage: &StorageHandle<'_>, id: U256) -> Result<CredisReservation> {
    take_reservation_if_held(storage, id)?
        .ok_or_else(|| VaultRouterError::ReservationNotFound(id).into())
}

/// Reads and deletes the reservation under `id`, or `None` when nothing is held.
fn take_reservation_if_held(
    storage: &StorageHandle<'_>,
    id: U256,
) -> Result<Option<CredisReservation>> {
    let contract = VaultRouterContract::new(storage.clone());
    let Some(record) = contract.reservations.get(id)? else {
        return Ok(None);
    };
    contract.reservations.delete(id)?;
    Ok(Some(record))
}

/// `getCredisReservation`: the reservation held under `id`, or a zeroed record when none.
pub fn credis_reservation_of(storage: &StorageHandle<'_>, id: U256) -> Result<CredisReservation> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(contract.reservations.get(id)?.unwrap_or(CredisReservation {
        id,
        ..Default::default()
    }))
}

/// Quote once, before withdrawing the reserved amount. Issuance never refreshes these terms.
fn quote(
    storage: &StorageHandle<'_>,
    asset: Address,
    amount: U256,
    reference_currency: u16,
) -> Result<CredisReservation> {
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
    let series_id = CredisContract::series_of(now, currency, reference_currency)
        .map_err(|_| VaultRouterError::InvalidAssetCurrency)?;
    Ok(CredisReservation {
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
