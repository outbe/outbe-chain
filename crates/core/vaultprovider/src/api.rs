//! Public Solidity ABI of the vaultprovider precompile, plus thin typed helpers
//! that hide the EVM sub-call to `VAULT_PROVIDER_ADDRESS`.

use alloy_primitives::{Address, U256};
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
