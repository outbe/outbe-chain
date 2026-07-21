//! ABI dispatch for the vaultprovider precompile at `VAULT_PROVIDER_ADDRESS`.

use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::types::StorageSet;
use outbe_primitives::storage::StorageHandle;

use crate::api::IVaultProvider;
use crate::crosschain;
use crate::runtime;
use crate::schema::VaultProviderContract;

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_call(
        data,
        IVaultProvider::IVaultProviderCalls::abi_decode,
        |call| {
            use IVaultProvider::IVaultProviderCalls::*;
            if !matches!(
                &call,
                crosschainDeposit(_) | crosschainWithdraw(_) | receiveMessage(_)
            ) {
                outbe_primitives::dispatch::reject_value(&value)?;
            }
            match call {
                // --- admin / metadata views ---
                owner(c) => view(c, |_c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    contract.owner.read()
                }),
                crosschainBridge(c) => view(c, |_c| {
                    VaultProviderContract::new(storage.clone())
                        .crosschain_bridge
                        .read()
                }),
                remoteVaultProvider(c) => view(c, |c| {
                    VaultProviderContract::new(storage.clone())
                        .remote_vault_providers
                        .read(&c.chainId)
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

                // --- fixed cross-chain peer configuration ---
                setCrosschainBridge(c) => mutate_void(c, caller, |sender, c| {
                    runtime::set_crosschain_bridge(storage.clone(), sender, c.bridge)
                }),
                setRemoteVaultProvider(c) => mutate_void(c, caller, |sender, c| {
                    runtime::set_remote_vault_provider(
                        storage.clone(),
                        sender,
                        c.chainId,
                        c.provider,
                    )
                }),
                // --- fixed cross-chain WCOEN vault flow ---
                crosschainAsset(c) => view(c, |_c| {
                    VaultProviderContract::new(storage.clone())
                        .crosschain_asset
                        .read()
                }),
                crosschainTokenBridge(c) => view(c, |_c| {
                    VaultProviderContract::new(storage.clone())
                        .crosschain_token_bridge
                        .read()
                }),
                crosschainDestinationChainId(c) => view(c, |_c| {
                    VaultProviderContract::new(storage.clone())
                        .crosschain_destination_chain_id
                        .read()
                }),
                setCrosschainAsset(c) => mutate_void(c, caller, |sender, c| {
                    crosschain::set_asset(
                        storage.clone(),
                        sender,
                        c.asset,
                        c.tokenBridge,
                        c.destinationChainId,
                    )
                }),
                crosschainOperationNonce(c) => view(c, |_c| {
                    VaultProviderContract::new(storage.clone())
                        .crosschain_operation_nonce
                        .read()
                }),
                pendingCrosschainOperations(c) => view(c, |_c| {
                    VaultProviderContract::new(storage.clone())
                        .pending_crosschain_operations
                        .read()
                }),
                crosschainShares(c) => view(c, |c| {
                    VaultProviderContract::new(storage.clone())
                        .crosschain_shares
                        .read(&c.user)
                }),
                totalCrosschainShares(c) => view(c, |_c| {
                    VaultProviderContract::new(storage.clone())
                        .total_crosschain_shares
                        .read()
                }),
                crosschainOperation(c) => view(c, |c| {
                    let contract = VaultProviderContract::new(storage.clone());
                    let kind = contract.operation_kinds.read(&c.operationId)?;
                    let status = contract.operation_statuses.read(&c.operationId)?;
                    Ok(IVaultProvider::crosschainOperationReturn {
                        user: contract.operation_users.read(&c.operationId)?,
                        amount: contract.operation_amounts.read(&c.operationId)?,
                        kind: IVaultProvider::CrosschainOperationKind::try_from(kind)
                            .unwrap_or(IVaultProvider::CrosschainOperationKind::Unknown),
                        status: IVaultProvider::CrosschainOperationStatus::try_from(status)
                            .unwrap_or(IVaultProvider::CrosschainOperationStatus::Unknown),
                    })
                }),
                quoteCrosschainDeposit(c) => view(c, |c| {
                    let (native_fee, operation_id) = crosschain::quote_deposit(
                        &storage,
                        caller,
                        c.assetsAmount,
                        c.destinationGasLimit,
                        c.acknowledgementGasLimit,
                    )?;
                    Ok(IVaultProvider::quoteCrosschainDepositReturn {
                        nativeFee: native_fee,
                        operationId: operation_id,
                    })
                }),
                crosschainDeposit(c) => mutate(c, caller, |user, c| {
                    let (operation_id, send_id) = crosschain::deposit(
                        storage.clone(),
                        user,
                        c.assetsAmount,
                        c.destinationGasLimit,
                        c.acknowledgementGasLimit,
                        value,
                    )?;
                    Ok(IVaultProvider::crosschainDepositReturn {
                        operationId: operation_id,
                        sendId: send_id,
                    })
                }),
                quoteCrosschainWithdraw(c) => view(c, |c| {
                    let (native_fee, operation_id) = crosschain::quote_withdraw(
                        &storage,
                        caller,
                        c.sharesAmount,
                        c.requestGasLimit,
                        c.returnGasLimit,
                    )?;
                    Ok(IVaultProvider::quoteCrosschainWithdrawReturn {
                        nativeFee: native_fee,
                        operationId: operation_id,
                    })
                }),
                crosschainWithdraw(c) => mutate(c, caller, |user, c| {
                    let (operation_id, send_id) = crosschain::withdraw(
                        storage.clone(),
                        user,
                        c.sharesAmount,
                        c.requestGasLimit,
                        c.returnGasLimit,
                        value,
                    )?;
                    Ok(IVaultProvider::crosschainWithdrawReturn {
                        operationId: operation_id,
                        sendId: send_id,
                    })
                }),
                receiveMessage(c) => mutate(c, caller, |_bridge, c| {
                    crosschain::receive_deposit_acknowledgement(
                        storage.clone(),
                        caller,
                        value,
                        &c.sender,
                        &c.payload,
                    )?;
                    Ok(FixedBytes::<4>::from_slice(
                        &IVaultProvider::receiveMessageCall::SELECTOR,
                    ))
                }),
                onCrosschainTokensReceived(c) => mutate(c, caller, |_token_bridge, c| {
                    crosschain::receive_withdrawal_return(
                        storage.clone(),
                        caller,
                        c.sourceDomain,
                        &c.from,
                        c.amount,
                        &c.extraData,
                    )?;
                    Ok(FixedBytes::<4>::from_slice(
                        &IVaultProvider::onCrosschainTokensReceivedCall::SELECTOR,
                    ))
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
