//! Public Solidity ABI of the vaultprovider precompile, plus thin typed helpers
//! that hide the EVM sub-call to `VAULT_PROVIDER_ADDRESS`.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall};

use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

sol!("../../../contracts/precompiles/src/IVaultProvider.sol");

/// `depositLiquidity`: deposit `amount` of `asset` into its reserve vault via an
/// EVM sub-call to the vault provider, returning the minted shares.
pub fn deposit_liquidity(
    storage: &StorageHandle<'_>,
    asset: Address,
    amount: U256,
) -> Result<U256> {
    let ret = storage.call(
        VAULT_PROVIDER_ADDRESS,
        U256::ZERO,
        IVaultProvider::depositLiquidityCall {
            asset,
            assetsAmount: amount,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultProvider::depositLiquidityCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("depositLiquidity undecodable".into()))
}

/// `withdrawLiquidity`: redeem `amount` of `asset` from its reserve vault and top
/// it up into `receiver` via an EVM sub-call to the vault provider, returning the
/// burned shares.
pub fn withdraw_liquidity(
    storage: &StorageHandle<'_>,
    asset: Address,
    amount: U256,
    receiver: Address,
) -> Result<U256> {
    let ret = storage.call(
        VAULT_PROVIDER_ADDRESS,
        U256::ZERO,
        IVaultProvider::withdrawLiquidityCall {
            asset,
            amount,
            receiver,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultProvider::withdrawLiquidityCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("withdrawLiquidity undecodable".into()))
}

/// Quotes a WCOEN deposit into the fixed remote vault and previews its
/// operation id.
pub fn quote_crosschain_deposit(
    storage: &StorageHandle<'_>,
    assets_amount: U256,
    destination_gas_limit: U256,
    acknowledgement_gas_limit: U256,
) -> Result<(U256, B256)> {
    let ret = storage.call(
        VAULT_PROVIDER_ADDRESS,
        U256::ZERO,
        IVaultProvider::quoteCrosschainDepositCall {
            assetsAmount: assets_amount,
            destinationGasLimit: destination_gas_limit,
            acknowledgementGasLimit: acknowledgement_gas_limit,
        }
        .abi_encode()
        .into(),
    )?;
    let decoded = IVaultProvider::quoteCrosschainDepositCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("quoteCrosschainDeposit undecodable".into()))?;
    Ok((decoded.nativeFee, decoded.operationId))
}

/// Locks WCOEN on Outbe and starts the fixed remote-vault deposit. `value`
/// must equal the current quoted native token-bridge fee.
pub fn crosschain_deposit(
    storage: &StorageHandle<'_>,
    assets_amount: U256,
    destination_gas_limit: U256,
    acknowledgement_gas_limit: U256,
    value: U256,
) -> Result<(B256, B256)> {
    let ret = storage.call(
        VAULT_PROVIDER_ADDRESS,
        value,
        IVaultProvider::crosschainDepositCall {
            assetsAmount: assets_amount,
            destinationGasLimit: destination_gas_limit,
            acknowledgementGasLimit: acknowledgement_gas_limit,
        }
        .abi_encode()
        .into(),
    )?;
    let decoded = IVaultProvider::crosschainDepositCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("crosschainDeposit undecodable".into()))?;
    Ok((decoded.operationId, decoded.sendId))
}

/// Quotes a 1:1 receipt-share withdrawal from the fixed remote vault and
/// previews its operation id.
pub fn quote_crosschain_withdraw(
    storage: &StorageHandle<'_>,
    shares_amount: U256,
    request_gas_limit: U256,
    return_gas_limit: U256,
) -> Result<(U256, B256)> {
    let ret = storage.call(
        VAULT_PROVIDER_ADDRESS,
        U256::ZERO,
        IVaultProvider::quoteCrosschainWithdrawCall {
            sharesAmount: shares_amount,
            requestGasLimit: request_gas_limit,
            returnGasLimit: return_gas_limit,
        }
        .abi_encode()
        .into(),
    )?;
    let decoded = IVaultProvider::quoteCrosschainWithdrawCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("quoteCrosschainWithdraw undecodable".into()))?;
    Ok((decoded.nativeFee, decoded.operationId))
}

/// Locks/burns the caller's mirrored 1:1 receipt shares and requests the
/// corresponding WCOEN back from the fixed remote vault. `value` must equal
/// the current quoted generic-bridge fee.
pub fn crosschain_withdraw(
    storage: &StorageHandle<'_>,
    shares_amount: U256,
    request_gas_limit: U256,
    return_gas_limit: U256,
    value: U256,
) -> Result<(B256, B256)> {
    let ret = storage.call(
        VAULT_PROVIDER_ADDRESS,
        value,
        IVaultProvider::crosschainWithdrawCall {
            sharesAmount: shares_amount,
            requestGasLimit: request_gas_limit,
            returnGasLimit: return_gas_limit,
        }
        .abi_encode()
        .into(),
    )?;
    let decoded = IVaultProvider::crosschainWithdrawCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("crosschainWithdraw undecodable".into()))?;
    Ok((decoded.operationId, decoded.sendId))
}
