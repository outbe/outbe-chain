//! ABI dispatch for the vaultprovider precompile at `VAULT_PROVIDER_ADDRESS`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::types::StorageSet;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::VaultProviderContract;

sol!("../../../contracts/precompiles/src/IVaultProvider.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        IVaultProvider::IVaultProviderCalls::abi_decode,
        |call| {
            use IVaultProvider::IVaultProviderCalls::*;
            match call {
                // --- admin / metadata views ---
                owner(c) => view(c, |_c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    contract.owner.read()
                }),

                // --- asset enumeration ---
                assetsCount(c) => view(c, |_c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    Ok(U256::from(contract.assets.len()?))
                }),
                assetAt(c) => view(c, |c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    set_at(&contract.assets, c.index)

                }),
                assetVaultsCount(c) => view(c, |c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    Ok(U256::from(contract.asset_vault_set(c.asset).len()?))
                }),
                assetVaultAt(c) => view(c, |c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    set_at(&contract.asset_vault_set(c.asset), c.index)
                }),

                // --- liquidity source / target enumeration ---
                liquiditySourcesCount(c) => view(c, |_c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    Ok(U256::from(contract.liquidity_sources.len()?))
                }),
                liquiditySourceAt(c) => view(c, |c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    let addr = set_at(&contract.liquidity_sources, c.index)?;
                    let type_u8 = contract.liquidity_source_types.read(&addr)?;
                    Ok(IVaultProvider::liquiditySourceAtReturn {
                        sourceAddress: addr,
                        sourceType: IVaultProvider::LiquiditySource::try_from(type_u8)
                            .unwrap_or(IVaultProvider::LiquiditySource::Unknown),
                    })
                }),
                liquidityTargetsCount(c) => view(c, |_c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    Ok(U256::from(contract.liquidity_targets.len()?))
                }),
                liquidityTargetAt(c) => view(c, |c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    let addr = set_at(&contract.liquidity_targets, c.index)?;
                    let type_u8 = contract.liquidity_target_types.read(&addr)?;
                    Ok(IVaultProvider::liquidityTargetAtReturn {
                        targetAddress: addr,
                        targetType: IVaultProvider::LiquidityTarget::try_from(type_u8)
                            .unwrap_or(IVaultProvider::LiquidityTarget::Unknown),
                    })
                }),

                // --- vault management (owner-only) ---
                addVault(c) => mutate_void(c, caller, |sender, c| {
                    runtime::add_vault(storage.clone(), sender, c.vault)
                }),
                removeVault(c) => mutate_void(c, caller, |sender, c| {
                    runtime::remove_vault(storage.clone(), sender, c.vault)
                }),

                // --- liquidity source / target management (owner-only) ---
                addLiquiditySource(c) => mutate_void(c, caller, |sender, c| {
                    runtime::add_liquidity_source(
                        storage.clone(),
                        sender,
                        c.sourceAddress,
                        c.sourceType as u8,
                    )
                }),
                removeLiquiditySource(c) => mutate_void(c, caller, |sender, c| {
                    runtime::remove_liquidity_source(storage.clone(), sender, c.sourceAddress)
                }),
                addLiquidityTarget(c) => mutate_void(c, caller, |sender, c| {
                    runtime::add_liquidity_target(
                        storage.clone(),
                        sender,
                        c.targetAddress,
                        c.targetType as u8,
                    )
                }),
                removeLiquidityTarget(c) => mutate_void(c, caller, |sender, c| {
                    runtime::remove_liquidity_target(storage.clone(), sender, c.targetAddress)
                }),

                // --- liquidity flow (source/target-gated against the registry) ---
                depositLiquidity(c) => mutate(c, caller, |sender, c| {
                    let source = runtime::registered_liquidity_source(&storage, sender)?;
                    runtime::deposit_liquidity(
                        storage.clone(),
                        sender,
                        c.asset,
                        c.assetsAmount,
                        source,
                    )
                }),
                withdrawLiquidity(c) => mutate(c, caller, |sender, c| {
                    let target = runtime::registered_liquidity_target(&storage, sender)?;
                    runtime::withdraw_liquidity(
                        storage.clone(),
                        sender,
                        c.asset,
                        c.amount,
                        c.receiver,
                        target,
                    )
                }),

                // --- views over external state ---
                sharesBalance(c) => view(c, |c| runtime::shares_balance(&storage, c.vault)),

                // --- VaultV2 gate hooks: only the provider itself is authorized ---
                canReceiveShares(c) => view(c, |c| Ok(c.account == VAULT_PROVIDER_ADDRESS)),
                canSendShares(c) => view(c, |c| Ok(c.account == VAULT_PROVIDER_ADDRESS)),
                canReceiveAssets(c) => view(c, |c| Ok(c.account == VAULT_PROVIDER_ADDRESS)),
                canSendAssets(c) => view(c, |c| Ok(c.account == VAULT_PROVIDER_ADDRESS)),
            }
        },
    )
}

/// Reads the set element at `index`, reverting (like OZ `EnumerableSet.at`) when
/// out of bounds.
fn set_at(set: &StorageSet<'_, Address>, index: U256) -> Result<Address> {
    let idx =
        u32::try_from(index).map_err(|_| PrecompileError::Revert("index out of bounds".into()))?;
    set.at(idx)?
        .ok_or_else(|| PrecompileError::Revert("index out of bounds".into()))
}
