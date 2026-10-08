//! Orchestration logic for the vaultrouter precompile.
//!
//! Faithful port of `contracts/.../VaultRouter.sol`. All cross-contract
//! interaction (ERC-20 token ops, ERC-4626 vault ops) goes
//! through `StorageHandle::call` / `StorageHandle::staticcall`. From the callee's
//! perspective `msg.sender` is `VAULT_ROUTER_ADDRESS` (this precompile).
//!
//! ERC-20 transfers reject false or malformed return values. They accept empty returns.

use alloy_primitives::{Address, U256};

use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::stablecoin::validate_currency_code;
use outbe_primitives::storage::StorageHandle;

use crate::api::{IVaultRouter, IVaultRouterCrosschainExtention};
use crate::errors::VaultRouterError;
use crate::schema::VaultRouterContract;

mod calls;
mod liquidity;
mod rebalance;
mod reservations;

use calls::{
    asset_iso_code, erc20_approve, erc20_balance_of, erc20_transfer, erc20_transfer_from,
    vault_asset, vault_deposit, vault_owner, vault_preview_withdraw, vault_withdraw,
};
pub use liquidity::{
    add_liquidity_source, add_liquidity_target, registered_liquidity_source,
    registered_liquidity_target, remove_liquidity_source, remove_liquidity_target,
};
pub(crate) use rebalance::{preview_rebalance, rebalance};
pub use reservations::reservation_of;
pub(crate) use reservations::{release_reservation, reserve_stables, return_reservation};

/// This precompile's own address (`address(this)` in the Solidity original).
const SELF: Address = VAULT_ROUTER_ADDRESS;

// ---------------------------------------------------------------------------
// owner gate
// ---------------------------------------------------------------------------

/// Reverts unless `sender` is the configured owner. Replaces `onlyOwner`.
fn ensure_owner(storage: &StorageHandle<'_>, sender: Address) -> Result<()> {
    let contract = VaultRouterContract::new(storage.clone());
    if contract.owner.read()? != sender {
        return Err(VaultRouterError::Unauthorized.into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cross-chain configuration
// ---------------------------------------------------------------------------

/// Configures the Outbe ERC-7786 bridge. A zero address disables the
/// cross-chain vault flow without affecting local liquidity operations.
pub fn set_crosschain_bridge(
    storage: StorageHandle<'_>,
    sender: Address,
    bridge: Address,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    ensure_no_pending_crosschain_operations(&storage)?;
    let mut contract = VaultRouterContract::new(storage);
    let old_bridge = contract.crosschain_bridge.read()?;
    contract.crosschain_bridge.write(bridge)?;
    contract.emit(IVaultRouterCrosschainExtention::CrosschainBridgeUpdated {
        oldBridge: old_bridge,
        newBridge: bridge,
    })
}

/// Registers the fixed vault adapter for a remote EVM chain. Passing the zero
/// address clears the peer while retaining the chain key's default value.
pub fn set_remote_vault_router(
    storage: StorageHandle<'_>,
    sender: Address,
    chain_id: U256,
    router: Address,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    ensure_no_pending_crosschain_operations(&storage)?;
    let local_chain_id = U256::from(storage.chain_id()?);
    if chain_id.is_zero() || chain_id == local_chain_id {
        return Err(VaultRouterError::InvalidDestinationChain.into());
    }

    let mut contract = VaultRouterContract::new(storage);
    let old_router = contract.remote_vault_routers.read(&chain_id)?;
    if router.is_zero() {
        contract.remote_vault_routers.clear(&chain_id)?;
    } else {
        contract.remote_vault_routers.write(&chain_id, router)?;
    }
    contract.emit(IVaultRouterCrosschainExtention::RemoteVaultRouterUpdated {
        chainId: chain_id,
        oldRouter: old_router,
        newRouter: router,
    })
}

fn ensure_no_pending_crosschain_operations(storage: &StorageHandle<'_>) -> Result<()> {
    let pending = VaultRouterContract::new(storage.clone())
        .pending_crosschain_operations
        .read()?;
    if !pending.is_zero() {
        return Err(VaultRouterError::CrosschainOperationsPending(pending).into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// vault management (owner-only)
// ---------------------------------------------------------------------------

/// Register an ownerless `vault` for its underlying asset and ISO 4217 reference currency.
/// The router grants the vault an unlimited allowance so the vault can transfer assets during
/// deposits.
pub fn add_vault(storage: StorageHandle<'_>, sender: Address, vault: Address) -> Result<()> {
    ensure_owner(&storage, sender)?;
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

    erc20_approve(&storage, asset, vault, U256::MAX)?;

    contract.emit(IVaultRouter::VaultAdded {
        isoCode: iso_code,
        asset,
        vault,
    })
}

/// Distinct assets whose vaults are registered under `iso_code`.
pub(crate) fn reference_currency_assets(
    storage: &StorageHandle<'_>,
    iso_code: u16,
) -> Result<Vec<Address>> {
    let contract = VaultRouterContract::new(storage.clone());
    let mut assets: Vec<Address> = Vec::new();
    for vault in contract.reference_currency_vault_set(iso_code).read_all()? {
        let asset = vault_asset(storage, vault)?;
        if !assets.contains(&asset) {
            assets.push(asset);
        }
    }
    Ok(assets)
}

/// `removeVault`: deregister `vault` for its asset and revoke the allowance.
pub fn remove_vault(storage: StorageHandle<'_>, sender: Address, vault: Address) -> Result<()> {
    ensure_owner(&storage, sender)?;
    if vault.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }

    let asset = vault_asset(&storage, vault)?;

    let mut contract = VaultRouterContract::new(storage.clone());
    let mut iso_code = contract.vault_reference_currencies.read(&vault)?;
    if iso_code == 0 {
        // Upgrade compatibility for vaults registered before the ISO index was
        // introduced: resolve their immutable asset metadata on first removal.
        iso_code = asset_iso_code(&storage, asset)?;
    }
    if !contract.asset_vault_set(asset).remove(&vault)? {
        return Err(VaultRouterError::ReserveVaultNotFound.into());
    }
    if contract.asset_vault_set(asset).is_empty()? {
        contract.assets.remove(&asset)?;
    }
    contract
        .reference_currency_vault_set(iso_code)
        .remove(&vault)?;
    contract.vault_reference_currencies.clear(&vault)?;

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
        assetsAmount: amount,
        sharesAmount: shares,
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
        amount,
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
        assetsAmount: amount,
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
