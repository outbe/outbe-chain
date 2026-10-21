//! Orchestration logic for the vaultrouter precompile.
//!
//! Faithful port of `contracts/.../VaultRouter.sol`. All cross-contract
//! interaction (ERC-20 token ops, ERC-4626 vault ops) goes
//! through `StorageHandle::call` / `StorageHandle::staticcall`. From the callee's
//! perspective `msg.sender` is `VAULT_ROUTER_ADDRESS` (this precompile).
//!
//! ERC-20 transfers and approvals reject false or malformed return values. They accept empty
//! returns.

use alloy_primitives::{Address, U256};

use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::stablecoin::validate_currency_code;
use outbe_primitives::storage::StorageHandle;

use crate::api::IVaultRouter;
use crate::errors::VaultRouterError;
use crate::schema::VaultRouterContract;

mod approved;
mod calls;
mod liquidity;
mod rebalance;
mod reservations;

pub(crate) use approved::{apply_approved, approved_vault_action};
use calls::{
    asset_iso_code, erc20_approve, erc20_balance_of, erc20_transfer, erc20_transfer_from,
    vault_asset, vault_deposit, vault_owner, vault_preview_withdraw, vault_withdraw,
};
pub(crate) use liquidity::{
    add_liquidity_source, add_liquidity_target, registered_liquidity_source,
    registered_liquidity_target, remove_liquidity_source, remove_liquidity_target,
};
pub(crate) use rebalance::{preview_rebalance, rebalance};
pub use reservations::reservation_of;
pub(crate) use reservations::{release_reservation, reserve_stables, return_reservation};

/// This precompile's own address (`address(this)` in the Solidity original).
const SELF: Address = VAULT_ROUTER_ADDRESS;

// ---------------------------------------------------------------------------
// governance-authorized vault management
// ---------------------------------------------------------------------------

/// Register an ownerless `vault` for its underlying asset and ISO 4217 reference currency.
/// The router grants the vault an unlimited allowance so the vault can transfer assets during
/// deposits.
pub(crate) fn add_vault(storage: StorageHandle<'_>, vault: Address) -> Result<()> {
    if vault.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }

    let asset = vault_asset(&storage, vault)?;
    if asset.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }

    let current_owner = vault_owner(&storage, vault)?;
    if !current_owner.is_zero() {
        return Err(VaultRouterError::ReserveVaultOwnerNotRenounced(current_owner).into());
    }

    let iso_code = asset_iso_code(&storage, asset)?;
    validate_currency_code(iso_code)?;

    let mut contract = VaultRouterContract::new(storage.clone());
    if !contract.asset_vault_set(asset).insert(vault)? {
        return Err(VaultRouterError::ReserveVaultAlreadyAdded.into());
    }
    contract.assets.insert(asset)?;
    contract
        .reference_currency_vault_set(iso_code)
        .insert(vault)?;
    contract
        .vault_reference_currencies
        .write(&vault, iso_code)?;
    contract.vault_assets.write(&vault, asset)?;

    erc20_approve(&storage, asset, vault, U256::MAX)?;

    contract.emit(IVaultRouter::VaultAdded {
        isoCode: iso_code,
        asset,
        vault,
    })
}

/// Distinct assets of the vaults at positions `[offset, offset + limit)` of the
/// `iso_code` vault set, in set order. Makes at most `limit` `asset()` sub-calls.
/// Assets are deduplicated within the page only. An `offset` past the end or a
/// zero `limit` returns an empty page. Values above `u32::MAX` saturate.
pub(crate) fn reference_currency_assets(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    offset: U256,
    limit: U256,
) -> Result<Vec<Address>> {
    let contract = VaultRouterContract::new(storage.clone());
    let vaults = contract.reference_currency_vault_set(iso_code);
    let offset = u32::try_from(offset).unwrap_or(u32::MAX);
    let limit = u32::try_from(limit).unwrap_or(u32::MAX);
    let end = offset.saturating_add(limit).min(vaults.len()?);
    let mut assets: Vec<Address> = Vec::new();
    for index in offset..end {
        let Some(vault) = vaults.at(index)? else {
            break;
        };
        let asset = vault_asset(storage, vault)?;
        if !assets.contains(&asset) {
            assets.push(asset);
        }
    }
    Ok(assets)
}

/// `removeVault`: deregister `vault` for its asset and revoke the allowance.
/// Requires the router's live share balance to be zero.
/// The asset comes from the registration record, so a vault whose `asset()`
/// changes or reverts stays removable.
pub(crate) fn remove_vault(storage: StorageHandle<'_>, vault: Address) -> Result<()> {
    if vault.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }

    let mut contract = VaultRouterContract::new(storage.clone());
    let mut asset = contract.vault_assets.read(&vault)?;
    if asset.is_zero() {
        // Upgrade compatibility for vaults registered before the asset record was
        // introduced: ask the vault for its asset on first removal.
        asset = vault_asset(&storage, vault)?;
    }
    let mut iso_code = contract.vault_reference_currencies.read(&vault)?;
    if iso_code == 0 {
        // Upgrade compatibility for vaults registered before the ISO index was
        // introduced: resolve their immutable asset metadata on first removal.
        iso_code = asset_iso_code(&storage, asset)?;
    }
    if !contract.asset_vault_set(asset).contains(&vault)? {
        return Err(VaultRouterError::ReserveVaultNotFound.into());
    }
    let shares = erc20_balance_of(&storage, vault, SELF)?;
    if !shares.is_zero() {
        return Err(VaultRouterError::ReserveVaultHasShares { vault, shares }.into());
    }
    contract.asset_vault_set(asset).remove(&vault)?;
    if contract.asset_vault_set(asset).is_empty()? {
        contract.assets.remove(&asset)?;
    }
    contract
        .reference_currency_vault_set(iso_code)
        .remove(&vault)?;
    contract.vault_reference_currencies.clear(&vault)?;
    contract.vault_assets.clear(&vault)?;

    erc20_approve(&storage, asset, vault, U256::ZERO)?;

    contract.emit(IVaultRouter::VaultRemoved {
        isoCode: iso_code,
        asset,
        vault,
    })
}

// ---------------------------------------------------------------------------
// liquidity flow
// ---------------------------------------------------------------------------

/// `deposit`: pulls `amount` of `asset` from the caller and deposits it
/// into the asset's vault. Returns the minted shares.
pub(crate) fn deposit(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    source: IVaultRouter::StablesSource,
) -> Result<U256> {
    if matches!(source, IVaultRouter::StablesSource::Unknown) {
        return Err(VaultRouterError::InvalidLiquiditySource.into());
    }

    let vault = first_vault(&storage, asset)?;

    erc20_transfer_from(&storage, asset, caller, SELF, amount)?;
    let shares = vault_deposit(&storage, vault, amount, SELF)?;

    let mut contract = VaultRouterContract::new(storage.clone());
    contract.emit(IVaultRouter::LiquidityDeposited {
        source: caller,
        vault,
        assetsMinor: amount,
        shares,
        sourceType: source,
    })?;

    Ok(shares)
}

/// `withdraw`: redeems assets and transfers them to `receiver`. Returns the burned shares.
pub(crate) fn withdraw(
    storage: StorageHandle<'_>,
    caller: Address,
    call: IVaultRouter::withdrawCall,
    target: IVaultRouter::StablesTarget,
) -> Result<U256> {
    let IVaultRouter::withdrawCall {
        asset,
        amountMinor: amount,
        receiver,
    } = call;
    if receiver.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }
    if matches!(target, IVaultRouter::StablesTarget::Unknown) {
        return Err(VaultRouterError::InvalidLiquidityTarget.into());
    }

    let vault = first_vault(&storage, asset)?;

    let required_shares = vault_preview_withdraw(&storage, vault, amount)?;
    let available_shares = erc20_balance_of(&storage, vault, SELF)?;
    if available_shares < required_shares {
        return Err(VaultRouterError::InsufficientSharesForWithdraw {
            available: available_shares,
            required: required_shares,
        }
        .into());
    }

    let burned_shares = vault_withdraw(&storage, vault, amount, SELF, SELF)?;

    erc20_transfer(&storage, asset, receiver, amount)?;

    let mut contract = VaultRouterContract::new(storage.clone());
    contract.emit(IVaultRouter::LiquidityWithdrawn {
        target: caller,
        receiver,
        vault,
        assetsMinor: amount,
        burnedShares: burned_shares,
        targetType: target,
    })?;

    Ok(burned_shares)
}

// ---------------------------------------------------------------------------
// views
// ---------------------------------------------------------------------------

/// `sharesBalance`: vault shares currently held by this router.
pub fn shares_balance(storage: &StorageHandle<'_>, vault: Address) -> Result<U256> {
    erc20_balance_of(storage, vault, SELF)
}

/// `hasLiquidity`: whether `asset`'s vault could currently fund `amount`. An asset
/// with no vault answers `false` rather than reverting.
pub fn has_liquidity(storage: &StorageHandle<'_>, asset: Address, amount: U256) -> Result<bool> {
    let contract = VaultRouterContract::new(storage.clone());
    let Some(vault) = contract.first_vault(asset)? else {
        return Ok(false);
    };
    let (required, available) = withdraw_shares(storage, vault, amount)?;
    Ok(available >= required)
}

/// Shares a withdrawal of `amount` from `vault` would burn, and the shares this
/// router actually holds.
fn withdraw_shares(
    storage: &StorageHandle<'_>,
    vault: Address,
    amount: U256,
) -> Result<(U256, U256)> {
    let required = vault_preview_withdraw(storage, vault, amount)?;
    let available = erc20_balance_of(storage, vault, SELF)?;
    Ok((required, available))
}

/// Rejects a draw of `amount` the router's shares in `vault` cannot cover.
fn ensure_shares_cover(storage: &StorageHandle<'_>, vault: Address, amount: U256) -> Result<()> {
    let (required, available) = withdraw_shares(storage, vault, amount)?;
    if available < required {
        return Err(VaultRouterError::InsufficientSharesForWithdraw {
            available,
            required,
        }
        .into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Resolves the first vault for `asset`, reverting if none is configured.
fn first_vault(storage: &StorageHandle<'_>, asset: Address) -> Result<Address> {
    let contract = VaultRouterContract::new(storage.clone());
    contract
        .first_vault(asset)?
        .ok_or_else(|| VaultRouterError::ReserveVaultNotConfigured.into())
}
