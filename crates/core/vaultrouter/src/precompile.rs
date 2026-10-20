//! ABI dispatch for the vaultrouter precompile at `VAULT_ROUTER_ADDRESS`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;

use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::types::StorageSet;
use outbe_primitives::storage::StorageHandle;

use crate::api::IVaultRouter;
use crate::runtime;
use crate::schema::VaultRouterContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_call(data, IVaultRouter::IVaultRouterCalls::abi_decode, |call| {
        use IVaultRouter::IVaultRouterCalls::*;
        outbe_primitives::dispatch::reject_value(&value)?;
        match call {
            // --- admin / metadata views ---
            owner(c) => view(c, |_c| {
                let contract = VaultRouterContract::new(storage.clone());
                contract.owner.read()
            }),

            // --- asset and reference-currency enumeration ---
            assetsCount(c) => view(c, |_c| {
                let contract = VaultRouterContract::new(storage.clone());
                Ok(U256::from(contract.assets.len()?))
            }),
            assetAt(c) => view(c, |c| {
                let contract = VaultRouterContract::new(storage.clone());
                set_at(&contract.assets, c.index)
            }),
            assetVaultsCount(c) => view(c, |c| {
                let contract = VaultRouterContract::new(storage.clone());
                Ok(U256::from(contract.asset_vault_set(c.asset).len()?))
            }),
            assetVaultAt(c) => view(c, |c| {
                let contract = VaultRouterContract::new(storage.clone());
                set_at(&contract.asset_vault_set(c.asset), c.index)
            }),
            referenceCurrencyVaultsCount(c) => view(c, |c| {
                let contract = VaultRouterContract::new(storage.clone());
                Ok(U256::from(
                    contract.reference_currency_vault_set(c.isoCode).len()?,
                ))
            }),
            referenceCurrencyVaultAt(c) => view(c, |c| {
                let contract = VaultRouterContract::new(storage.clone());
                set_at(&contract.reference_currency_vault_set(c.isoCode), c.index)
            }),
            vaultReferenceCurrency(c) => view(c, |c| {
                VaultRouterContract::new(storage.clone())
                    .vault_reference_currencies
                    .read(&c.vault)
            }),
            referenceCurrencyAssets(c) => view(c, |c| {
                runtime::reference_currency_assets(&storage, c.isoCode, c.offset, c.limit)
            }),

            // --- liquidity source / target enumeration ---
            liquiditySourcesCount(c) => view(c, |_c| {
                let contract = VaultRouterContract::new(storage.clone());
                Ok(U256::from(contract.liquidity_sources.len()?))
            }),
            liquiditySourceAt(c) => view(c, |c| {
                let contract = VaultRouterContract::new(storage.clone());
                let addr = set_at(&contract.liquidity_sources, c.index)?;
                let type_u8 = contract.liquidity_source_types.read(&addr)?;
                Ok(IVaultRouter::liquiditySourceAtReturn {
                    sourceAddress: addr,
                    sourceType: IVaultRouter::StablesSource::try_from(type_u8)
                        .unwrap_or(IVaultRouter::StablesSource::Unknown),
                })
            }),
            liquidityTargetsCount(c) => view(c, |_c| {
                let contract = VaultRouterContract::new(storage.clone());
                Ok(U256::from(contract.liquidity_targets.len()?))
            }),
            liquidityTargetAt(c) => view(c, |c| {
                let contract = VaultRouterContract::new(storage.clone());
                let addr = set_at(&contract.liquidity_targets, c.index)?;
                let type_u8 = contract.liquidity_target_types.read(&addr)?;
                Ok(IVaultRouter::liquidityTargetAtReturn {
                    targetAddress: addr,
                    targetType: IVaultRouter::StablesTarget::try_from(type_u8)
                        .unwrap_or(IVaultRouter::StablesTarget::Unknown),
                })
            }),

            // --- vault management (owner-only) ---
            addVault(c) => mutate_void(&storage, c, caller, |sender, c| {
                runtime::add_vault(storage.clone(), sender, c.vault)
            }),
            removeVault(c) => mutate_void(&storage, c, caller, |sender, c| {
                runtime::remove_vault(storage.clone(), sender, c.vault)
            }),

            // --- liquidity source / target management (owner-only) ---
            addLiquiditySource(c) => mutate_void(&storage, c, caller, |sender, c| {
                runtime::add_liquidity_source(
                    storage.clone(),
                    sender,
                    c.sourceAddress,
                    c.sourceType as u8,
                )
            }),
            removeLiquiditySource(c) => mutate_void(&storage, c, caller, |sender, c| {
                runtime::remove_liquidity_source(storage.clone(), sender, c.sourceAddress)
            }),
            addLiquidityTarget(c) => mutate_void(&storage, c, caller, |sender, c| {
                runtime::add_liquidity_target(
                    storage.clone(),
                    sender,
                    c.targetAddress,
                    c.targetType as u8,
                )
            }),
            removeLiquidityTarget(c) => mutate_void(&storage, c, caller, |sender, c| {
                runtime::remove_liquidity_target(storage.clone(), sender, c.targetAddress)
            }),

            // --- liquidity flow (source/target-gated against the registry) ---
            deposit(c) => mutate(&storage, c, caller, |sender, c| {
                let source = runtime::registered_liquidity_source(&storage, sender)?;
                runtime::deposit(storage.clone(), sender, c.asset, c.assetsMinor, source)
            }),
            withdraw(c) => mutate(&storage, c, caller, |sender, c| {
                let target = runtime::registered_liquidity_target(&storage, sender)?;
                runtime::withdraw(storage.clone(), sender, c, target)
            }),

            // --- views over external state ---
            sharesBalance(c) => view(c, |c| runtime::shares_balance(&storage, c.vault)),
            hasLiquidity(c) => view(c, |c| {
                runtime::has_liquidity(&storage, c.asset, c.amountMinor)
            }),
            reservationOf(c) => view(c, |c| {
                let record = runtime::reservation_of(&storage, c.id)?;
                Ok(record.into())
            }),

            // --- reservations ---
            reserveStables(c) => mutate(&storage, c, caller, |sender, c| {
                runtime::reserve_stables(storage.clone(), sender, c)
            }),
            releaseReservation(c) => mutate(&storage, c, caller, |sender, c| {
                if sender != outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS {
                    return Err(crate::errors::VaultRouterError::Unauthorized.into());
                }
                let target = runtime::registered_liquidity_target(&storage, sender)?;
                runtime::release_reservation(
                    storage.clone(),
                    c.id,
                    c.receiver,
                    c.amountMinor,
                    target,
                )
            }),
            returnReservation(c) => mutate(&storage, c, caller, |sender, c| {
                runtime::return_reservation(storage.clone(), sender, c.id)
            }),

            // --- rebalance (CCA-gated; caller supplies the destination asset) ---
            rebalance(c) => mutate(&storage, c, caller, |sender, c| {
                runtime::rebalance(storage.clone(), sender, c)
            }),
            previewRebalance(c) => view(c, |c| {
                let (asset_from, asset_to, amount_to) =
                    runtime::preview_rebalance(&storage, c.vaultFrom, c.vaultTo, c.assetsMinor)?;
                Ok(IVaultRouter::previewRebalanceReturn {
                    assetFrom: asset_from,
                    assetTo: asset_to,
                    assetsToMinor: amount_to,
                })
            }),
        }
    })
}

/// Reads the set element at `index`. Reverts (like OZ `EnumerableSet.at`) when
/// `index` is out of bounds.
fn set_at(set: &StorageSet<'_, Address>, index: U256) -> Result<Address> {
    let idx =
        u32::try_from(index).map_err(|_| crate::errors::VaultRouterError::IndexOutOfBounds)?;
    set.at(idx)?
        .ok_or_else(|| crate::errors::VaultRouterError::IndexOutOfBounds.into())
}
