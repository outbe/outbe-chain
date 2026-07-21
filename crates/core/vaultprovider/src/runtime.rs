//! Orchestration logic for the vaultprovider precompile.
//!
//! Faithful port of `contracts/.../VaultProvider.sol`. All cross-contract
//! interaction (ERC-20 token ops, ERC-4626 vault ops, token-bundle top-up) goes
//! through `StorageHandle::call` / `StorageHandle::staticcall`; from the callee's
//! perspective `msg.sender` is `VAULT_PROVIDER_ADDRESS` (this precompile).
//!
//! Following the repo convention (see `outbe_credisfactory::runtime`), ERC-20
//! mutating sub-calls propagate failure by reverting; their boolean return is
//! not separately decoded.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;

use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::api::IVaultProvider;
use crate::errors::VaultProviderError;
use crate::schema::{VaultProviderContract, UNKNOWN};
use crate::sol_ext::{ITokenBundle, IVaultV2, IERC20};

/// This precompile's own address (`address(this)` in the Solidity original).
const SELF: Address = VAULT_PROVIDER_ADDRESS;

// ---------------------------------------------------------------------------
// owner gate
// ---------------------------------------------------------------------------

/// Reverts unless `sender` is the configured owner. Replaces `onlyOwner`.
fn ensure_owner(storage: &StorageHandle<'_>, sender: Address) -> Result<()> {
    let contract = VaultProviderContract::new(storage.clone());
    if contract.owner.read()? != sender {
        return Err(VaultProviderError::Unauthorized.into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// vault management (owner-only)
// ---------------------------------------------------------------------------

/// `addVault`: register `vault` for its underlying asset and grant the provider
/// an unlimited allowance so the vault can pull on deposit.
pub fn add_vault(storage: StorageHandle<'_>, sender: Address, vault: Address) -> Result<()> {
    ensure_owner(&storage, sender)?;
    if vault.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }

    let asset = vault_asset(&storage, vault)?;
    if asset.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }

    let mut contract = VaultProviderContract::new(storage.clone());
    if !contract.asset_vault_set(asset).insert(vault)? {
        return Err(VaultProviderError::ReserveVaultAlreadyAdded.into());
    }
    contract.assets.insert(asset)?;

    erc20_approve(&storage, asset, vault, U256::MAX)?;

    contract.emit(IVaultProvider::VaultAdded { asset, vault })
}

/// `removeVault`: deregister `vault` for its asset and revoke the allowance.
pub fn remove_vault(storage: StorageHandle<'_>, sender: Address, vault: Address) -> Result<()> {
    ensure_owner(&storage, sender)?;
    if vault.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }

    let asset = vault_asset(&storage, vault)?;

    let mut contract = VaultProviderContract::new(storage.clone());
    if !contract.asset_vault_set(asset).remove(&vault)? {
        return Err(VaultProviderError::ReserveVaultNotFound.into());
    }
    if contract.asset_vault_set(asset).is_empty()? {
        contract.assets.remove(&asset)?;
    }

    erc20_approve(&storage, asset, vault, U256::ZERO)?;

    contract.emit(IVaultProvider::VaultRemoved { asset, vault })
}

// ---------------------------------------------------------------------------
// liquidity source / target management (owner-only)
// ---------------------------------------------------------------------------

pub fn add_liquidity_source(
    storage: StorageHandle<'_>,
    sender: Address,
    source: Address,
    source_type: u8,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    if source.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }
    if source_type == UNKNOWN {
        return Err(VaultProviderError::InvalidLiquiditySource.into());
    }

    let mut contract = VaultProviderContract::new(storage.clone());
    contract.liquidity_sources.insert(source)?;
    contract
        .liquidity_source_types
        .write(&source, source_type)?;

    contract.emit(IVaultProvider::LiquiditySourceAdded {
        sourceAddress: source,
        sourceType: liquidity_source(source_type),
    })
}

pub fn remove_liquidity_source(
    storage: StorageHandle<'_>,
    sender: Address,
    source: Address,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    let mut contract = VaultProviderContract::new(storage.clone());
    if !contract.liquidity_sources.remove(&source)? {
        return Err(VaultProviderError::LiquiditySourceNotFound.into());
    }
    let source_type = contract.liquidity_source_types.read(&source)?;
    contract.liquidity_source_types.clear(&source)?;

    contract.emit(IVaultProvider::LiquiditySourceRemoved {
        sourceAddress: source,
        sourceType: liquidity_source(source_type),
    })
}

pub fn add_liquidity_target(
    storage: StorageHandle<'_>,
    sender: Address,
    target: Address,
    target_type: u8,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    if target.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }
    if target_type == UNKNOWN {
        return Err(VaultProviderError::InvalidLiquidityTarget.into());
    }

    let mut contract = VaultProviderContract::new(storage.clone());
    contract.liquidity_targets.insert(target)?;
    contract
        .liquidity_target_types
        .write(&target, target_type)?;

    contract.emit(IVaultProvider::LiquidityTargetAdded {
        targetAddress: target,
        targetType: liquidity_target(target_type),
    })
}

pub fn remove_liquidity_target(
    storage: StorageHandle<'_>,
    sender: Address,
    target: Address,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    let mut contract = VaultProviderContract::new(storage.clone());
    if !contract.liquidity_targets.remove(&target)? {
        return Err(VaultProviderError::LiquidityTargetNotFound.into());
    }
    let target_type = contract.liquidity_target_types.read(&target)?;
    contract.liquidity_target_types.clear(&target)?;

    contract.emit(IVaultProvider::LiquidityTargetRemoved {
        targetAddress: target,
        targetType: liquidity_target(target_type),
    })
}

// ---------------------------------------------------------------------------
// liquidity flow
// ---------------------------------------------------------------------------

/// Resolves the `LiquiditySource` registered for `caller`, returning `Unknown`
/// when `caller` is not a registered source.
pub fn registered_liquidity_source(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultProvider::LiquiditySource> {
    let contract = VaultProviderContract::new(storage.clone());
    Ok(liquidity_source(
        contract.liquidity_source_types.read(&caller)?,
    ))
}

/// Resolves the `LiquidityTarget` registered for `caller`, returning `Unknown`
/// when `caller` is not a registered target.
pub fn registered_liquidity_target(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultProvider::LiquidityTarget> {
    let contract = VaultProviderContract::new(storage.clone());
    Ok(liquidity_target(
        contract.liquidity_target_types.read(&caller)?,
    ))
}

/// `depositLiquidity`: pulls `amount` of `asset` from the caller and deposits it
/// into the asset's vault, returning the minted shares.
pub(crate) fn deposit_liquidity(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    source: IVaultProvider::LiquiditySource,
) -> Result<U256> {
    if matches!(source, IVaultProvider::LiquiditySource::Unknown) {
        return Err(VaultProviderError::InvalidLiquiditySource.into());
    }

    let vault = first_vault(&storage, asset)?;

    erc20_transfer_from(&storage, asset, caller, SELF, amount)?;
    let shares = vault_deposit(&storage, vault, amount, SELF)?;

    let mut contract = VaultProviderContract::new(storage.clone());
    contract.emit(IVaultProvider::LiquidityDeposited {
        source: caller,
        vault,
        assetsAmount: amount,
        sharesAmount: shares,
        sourceType: source,
    })?;

    Ok(shares)
}

/// `withdrawLiquidity`: redeems `amount` of `asset` from the vault and tops it
/// up into `receiver` (a token bundle), returning the burned shares.
pub(crate) fn withdraw_liquidity(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    receiver: Address,
    target: IVaultProvider::LiquidityTarget,
) -> Result<U256> {
    if receiver.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }
    if matches!(target, IVaultProvider::LiquidityTarget::Unknown) {
        return Err(VaultProviderError::InvalidLiquidityTarget.into());
    }

    let vault = first_vault(&storage, asset)?;

    let required_shares = vault_preview_withdraw(&storage, vault, amount)?;
    let available_shares = erc20_balance_of(&storage, vault, SELF)?;
    if available_shares < required_shares {
        return Err(VaultProviderError::InsufficientSharesForWithdraw {
            available: available_shares,
            required: required_shares,
        }
        .into());
    }

    let burned_shares = vault_withdraw(&storage, vault, amount, SELF, SELF)?;

    erc20_approve(&storage, asset, receiver, amount)?;
    token_bundle_top_up(&storage, receiver, SELF, asset, amount)?;

    let mut contract = VaultProviderContract::new(storage.clone());
    contract.emit(IVaultProvider::LiquidityWithdrawn {
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

/// `sharesBalance`: vault shares currently held by this provider.
pub fn shares_balance(storage: &StorageHandle<'_>, vault: Address) -> Result<U256> {
    erc20_balance_of(storage, vault, SELF)
}

// ---------------------------------------------------------------------------
// helpers: enum reconstruction
// ---------------------------------------------------------------------------

fn liquidity_source(value: u8) -> IVaultProvider::LiquiditySource {
    IVaultProvider::LiquiditySource::try_from(value)
        .unwrap_or(IVaultProvider::LiquiditySource::Unknown)
}

fn liquidity_target(value: u8) -> IVaultProvider::LiquidityTarget {
    IVaultProvider::LiquidityTarget::try_from(value)
        .unwrap_or(IVaultProvider::LiquidityTarget::Unknown)
}

/// Resolves the first vault for `asset`, reverting if none is configured.
fn first_vault(storage: &StorageHandle<'_>, asset: Address) -> Result<Address> {
    let contract = VaultProviderContract::new(storage.clone());
    contract
        .first_vault(asset)?
        .ok_or_else(|| VaultProviderError::ReserveVaultNotConfigured.into())
}

// ---------------------------------------------------------------------------
// helpers: external sub-calls
// ---------------------------------------------------------------------------

fn erc20_approve(
    storage: &StorageHandle<'_>,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    let calldata = IERC20::approveCall { spender, amount }.abi_encode();
    storage.call(token, U256::ZERO, calldata.into())?;
    Ok(())
}

fn erc20_transfer_from(
    storage: &StorageHandle<'_>,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
) -> Result<()> {
    let calldata = IERC20::transferFromCall { from, to, amount }.abi_encode();
    storage.call(token, U256::ZERO, calldata.into())?;
    Ok(())
}

fn erc20_balance_of(storage: &StorageHandle<'_>, token: Address, account: Address) -> Result<U256> {
    let ret = storage.staticcall(token, IERC20::balanceOfCall { account }.abi_encode().into())?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| VaultProviderError::UndecodableReturn("ERC20 balanceOf").into())
}

fn vault_asset(storage: &StorageHandle<'_>, vault: Address) -> Result<Address> {
    let ret = storage.staticcall(vault, IVaultV2::assetCall {}.abi_encode().into())?;
    IVaultV2::assetCall::abi_decode_returns(&ret)
        .map_err(|_| VaultProviderError::UndecodableReturn("IVaultV2 asset").into())
}

fn vault_deposit(
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
        .map_err(|_| VaultProviderError::UndecodableReturn("IVaultV2 deposit").into())
}

fn vault_preview_withdraw(
    storage: &StorageHandle<'_>,
    vault: Address,
    assets: U256,
) -> Result<U256> {
    let ret = storage.staticcall(
        vault,
        IVaultV2::previewWithdrawCall { assets }.abi_encode().into(),
    )?;
    IVaultV2::previewWithdrawCall::abi_decode_returns(&ret)
        .map_err(|_| VaultProviderError::UndecodableReturn("IVaultV2 previewWithdraw").into())
}

fn vault_withdraw(
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
        .map_err(|_| VaultProviderError::UndecodableReturn("IVaultV2 withdraw").into())
}

fn token_bundle_top_up(
    storage: &StorageHandle<'_>,
    receiver: Address,
    sender: Address,
    token: Address,
    amount: U256,
) -> Result<()> {
    // A CALL to a codeless account succeeds and returns empty in EVM, so topUp's
    // internal guards would be silently skipped if the bundle smart account is not
    // deployed. Reject up front so requestCredis fails instead of half-completing.
    if storage.with_account_info(receiver, |info| Ok(info.is_empty_code_hash()))? {
        return Err(VaultProviderError::ReceiverNotDeployed.into());
    }
    let calldata = ITokenBundle::topUpCall {
        sender,
        token,
        amount,
    }
    .abi_encode();
    storage.call(receiver, U256::ZERO, calldata.into())?;
    Ok(())
}
