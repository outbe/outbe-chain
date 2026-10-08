//! CCA-gated liquidity moves between registered reserve vaults.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::stablecoin::validate_currency_code;
use outbe_primitives::storage::StorageHandle;

use super::calls::{
    asset_iso_code, erc20_balance_of, erc20_decimals, erc20_transfer, erc20_transfer_from,
    vault_asset, vault_deposit, vault_preview_withdraw, vault_withdraw,
};
use super::SELF;
use crate::api::IVaultRouter;
use crate::errors::VaultRouterError;
use crate::schema::VaultRouterContract;

/// Ceiling on a registered asset's `decimals()` accepted by `rebalance`. Every scaling
/// below this bound is an exact power-of-ten multiply or a single ceiling divide.
const MAX_ASSET_DECIMALS: u8 = 18;

/// `rebalance`: moves `amount` of liquidity from `vault_from` to `vault_to`. The caller
/// supplies `asset_to` (the destination vault's underlying asset) at the oracle cross rate
/// and receives `asset_from` in return. As a result, the router never holds a standing
/// allowance and never sources liquidity itself. The caller must have approved this router
/// for at least the required amount beforehand. `max_amount_to` bounds what the router may
/// pull if the rate moved between the caller's quote and this call.
pub(crate) fn rebalance(
    storage: StorageHandle<'_>,
    caller: Address,
    call: IVaultRouter::rebalanceCall,
) -> Result<U256> {
    let IVaultRouter::rebalanceCall {
        vaultFrom: vault_from,
        vaultTo: vault_to,
        assetsAmount: amount,
        maxAmountTo: max_amount_to,
    } = call;
    outbe_ccaregistry::api::require_active_cca(&storage, caller)?;
    if vault_from == vault_to {
        return Err(VaultRouterError::SameVaultRebalance.into());
    }
    if amount.is_zero() {
        return Err(VaultRouterError::InvalidRebalanceAmount.into());
    }

    let (asset_from, asset_to) = registered_rebalance_assets(&storage, vault_from, vault_to)?;
    let amount_to = rebalance_amount_to(&storage, asset_from, asset_to, amount)?;
    if amount_to > max_amount_to {
        return Err(VaultRouterError::RebalanceInputExceedsMax {
            required: amount_to,
            max_amount_to,
        }
        .into());
    }

    let required_shares = vault_preview_withdraw(&storage, vault_from, amount)?;
    let available_shares = erc20_balance_of(&storage, vault_from, SELF)?;
    if available_shares < required_shares {
        return Err(VaultRouterError::InsufficientSharesForWithdraw {
            available: available_shares,
            required: required_shares,
        }
        .into());
    }

    storage.with_checkpoint(|| {
        // Receive before paying: an unapproved or short caller reverts here, before either
        // vault is touched.
        erc20_transfer_from(&storage, asset_to, caller, SELF, amount_to)?;
        let minted = vault_deposit(&storage, vault_to, amount_to, SELF)?;

        let burned = vault_withdraw(&storage, vault_from, amount, SELF, SELF)?;
        erc20_transfer(&storage, asset_from, caller, amount)?;

        let mut contract = VaultRouterContract::new(storage.clone());
        contract.emit(IVaultRouter::LiquidityRebalanced {
            cca: caller,
            vaultFrom: vault_from,
            vaultTo: vault_to,
            assetsWithdrawn: amount,
            burnedShares: burned,
            assetsDeposited: amount_to,
            mintedShares: minted,
        })?;
        Ok(amount_to)
    })
}

/// `previewRebalance`: what a `rebalance` of `amount` from `vault_from` to `vault_to` would
/// require the caller to supply. The caller can then approve exactly that before calling.
pub(crate) fn preview_rebalance(
    storage: &StorageHandle<'_>,
    vault_from: Address,
    vault_to: Address,
    amount: U256,
) -> Result<(Address, Address, U256)> {
    if vault_from == vault_to {
        return Err(VaultRouterError::SameVaultRebalance.into());
    }
    let (asset_from, asset_to) = registered_rebalance_assets(storage, vault_from, vault_to)?;
    let amount_to = rebalance_amount_to(storage, asset_from, asset_to, amount)?;
    Ok((asset_from, asset_to, amount_to))
}

/// Resolves both vaults' underlying assets and confirms each vault is still registered
/// under its own asset. The check uses the enumerable set membership that
/// `addVault`/`removeVault` maintain. It does not use `vault_reference_currencies`, which
/// has an upgrade-compatibility hole for vaults registered before the ISO index existed
/// (see `remove_vault` above).
fn registered_rebalance_assets(
    storage: &StorageHandle<'_>,
    vault_from: Address,
    vault_to: Address,
) -> Result<(Address, Address)> {
    let asset_from = vault_asset(storage, vault_from)?;
    let asset_to = vault_asset(storage, vault_to)?;
    let contract = VaultRouterContract::new(storage.clone());
    if !contract.asset_vault_set(asset_from).contains(&vault_from)? {
        return Err(VaultRouterError::RebalanceVaultNotRegistered(vault_from).into());
    }
    if !contract.asset_vault_set(asset_to).contains(&vault_to)? {
        return Err(VaultRouterError::RebalanceVaultNotRegistered(vault_to).into());
    }
    Ok((asset_from, asset_to))
}

/// `amount` of `asset_from` re-expressed in `asset_to`. Identical assets short-circuit at
/// 1:1 with no oracle read and no decimal scaling. Otherwise the two assets' ISO 4217
/// currencies are converted through the oracle's COEN cross rate.
fn rebalance_amount_to(
    storage: &StorageHandle<'_>,
    asset_from: Address,
    asset_to: Address,
    amount: U256,
) -> Result<U256> {
    if asset_from == asset_to {
        return Ok(amount);
    }

    let decimals_from = erc20_decimals(storage, asset_from)?;
    let decimals_to = erc20_decimals(storage, asset_to)?;
    if decimals_from > MAX_ASSET_DECIMALS {
        return Err(VaultRouterError::UnsupportedAssetDecimals(decimals_from).into());
    }
    if decimals_to > MAX_ASSET_DECIMALS {
        return Err(VaultRouterError::UnsupportedAssetDecimals(decimals_to).into());
    }

    let iso_from = asset_iso_code(storage, asset_from)?;
    let iso_to = asset_iso_code(storage, asset_to)?;
    validate_currency_code(iso_from)?;
    validate_currency_code(iso_to)?;
    if decimals_to >= decimals_from {
        let scaled = rescale_decimals(amount, decimals_from, decimals_to)?;
        outbe_oracle::api::fresh_currency_cross_rate(storage.clone(), iso_from, iso_to, scaled)
    } else {
        let converted = outbe_oracle::api::fresh_currency_cross_rate(
            storage.clone(),
            iso_from,
            iso_to,
            amount,
        )?;
        rescale_decimals(converted, decimals_from, decimals_to)
    }
}

/// Rescales `amount` from `from_decimals` to `to_decimals`. Scaling up is an exact
/// power-of-ten multiply. Scaling down rounds up.
fn rescale_decimals(amount: U256, from_decimals: u8, to_decimals: u8) -> Result<U256> {
    match to_decimals.cmp(&from_decimals) {
        core::cmp::Ordering::Equal => Ok(amount),
        core::cmp::Ordering::Greater => amount
            .checked_mul(U256::from(10u64).pow(U256::from(to_decimals - from_decimals)))
            .ok_or_else(|| VaultRouterError::InvalidRebalanceAmount.into()),
        core::cmp::Ordering::Less => {
            Ok(amount.div_ceil(U256::from(10u64).pow(U256::from(from_decimals - to_decimals))))
        }
    }
}
