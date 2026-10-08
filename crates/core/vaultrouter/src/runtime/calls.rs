//! ERC-20 and ERC-4626 sub-calls. From the callee's perspective `msg.sender` is this precompile.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::VaultRouterError;
use crate::sol_ext::{IReferenceCurrency, IVaultV2, IERC20};

pub(super) fn erc20_approve(
    storage: &StorageHandle<'_>,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    let calldata = IERC20::approveCall { spender, amount }.abi_encode();
    storage.call(token, U256::ZERO, calldata.into())?;
    Ok(())
}

pub(super) fn erc20_transfer_from(
    storage: &StorageHandle<'_>,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
) -> Result<()> {
    let calldata = IERC20::transferFromCall { from, to, amount }.abi_encode();
    let ret = storage.call(token, U256::ZERO, calldata.into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(VaultRouterError::TokenOperationFailed.into());
    }
    Ok(())
}

pub(super) fn erc20_balance_of(
    storage: &StorageHandle<'_>,
    token: Address,
    account: Address,
) -> Result<U256> {
    let ret = storage.staticcall(token, IERC20::balanceOfCall { account }.abi_encode().into())?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("ERC20 balanceOf").into())
}

pub(super) fn erc20_decimals(storage: &StorageHandle<'_>, token: Address) -> Result<u8> {
    let ret = storage.staticcall(token, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("ERC20 decimals").into())
}

pub(super) fn erc20_transfer(
    storage: &StorageHandle<'_>,
    token: Address,
    to: Address,
    amount: U256,
) -> Result<()> {
    let calldata = IERC20::transferCall { to, amount }.abi_encode();
    let ret = storage.call(token, U256::ZERO, calldata.into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(VaultRouterError::TokenOperationFailed.into());
    }
    Ok(())
}

pub(super) fn vault_asset(storage: &StorageHandle<'_>, vault: Address) -> Result<Address> {
    let ret = storage.staticcall(vault, IVaultV2::assetCall {}.abi_encode().into())?;
    IVaultV2::assetCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 asset").into())
}

pub(super) fn vault_owner(storage: &StorageHandle<'_>, vault: Address) -> Result<Address> {
    let ret = storage.staticcall(vault, IVaultV2::ownerCall {}.abi_encode().into())?;
    IVaultV2::ownerCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 owner").into())
}

pub(super) fn asset_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IReferenceCurrency isoCode").into())
}

pub(super) fn vault_deposit(
    storage: &StorageHandle<'_>,
    vault: Address,
    assets: U256,
    on_behalf: Address,
) -> Result<U256> {
    let ret = storage.call(
        vault,
        U256::ZERO,
        IVaultV2::depositCall {
            assets,
            onBehalf: on_behalf,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultV2::depositCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 deposit").into())
}

pub(super) fn vault_preview_withdraw(
    storage: &StorageHandle<'_>,
    vault: Address,
    assets: U256,
) -> Result<U256> {
    let ret = storage.staticcall(
        vault,
        IVaultV2::previewWithdrawCall { assets }.abi_encode().into(),
    )?;
    IVaultV2::previewWithdrawCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 previewWithdraw").into())
}

pub(super) fn vault_withdraw(
    storage: &StorageHandle<'_>,
    vault: Address,
    assets: U256,
    receiver: Address,
    on_behalf: Address,
) -> Result<U256> {
    let ret = storage.call(
        vault,
        U256::ZERO,
        IVaultV2::withdrawCall {
            assets,
            receiver,
            onBehalf: on_behalf,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultV2::withdrawCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 withdraw").into())
}
