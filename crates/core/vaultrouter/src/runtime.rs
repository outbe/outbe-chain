//! Orchestration logic for the vaultrouter precompile.
//!
//! Faithful port of `contracts/.../VaultRouter.sol`. All cross-contract
//! interaction (ERC-20 token ops, ERC-4626 vault ops, token-bundle top-up) goes
//! through `StorageHandle::call` / `StorageHandle::staticcall`; from the callee's
//! perspective `msg.sender` is `VAULT_ROUTER_ADDRESS` (this precompile).
//!
//! Following the repo convention (see `outbe_credisfactory::runtime`), ERC-20
//! mutating sub-calls propagate failure by reverting; their boolean return is
//! not separately decoded.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;

use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::api::{IVaultRouter, IVaultRouterCrosschainExtention};
use crate::errors::VaultRouterError;
use crate::schema::{VaultRouterContract, UNKNOWN};
use crate::sol_ext::IReferenceCurrency;
use crate::sol_ext::{ITokenBundle, IVaultV2, IERC20};

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

/// `addVault`: register an ownerless `vault` for its underlying asset and ISO
/// 4217 reference currency, then grant the router an unlimited allowance so
/// the vault can pull on deposit.
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
    if iso_code == 0 {
        return Err(VaultRouterError::InvalidReferenceCurrency.into());
    }

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
        return Err(VaultRouterError::ZeroAddress.into());
    }
    if source_type == UNKNOWN {
        return Err(VaultRouterError::InvalidLiquiditySource.into());
    }

    let mut contract = VaultRouterContract::new(storage.clone());
    contract.liquidity_sources.insert(source)?;
    contract
        .liquidity_source_types
        .write(&source, source_type)?;

    contract.emit(IVaultRouter::LiquiditySourceAdded {
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
    let mut contract = VaultRouterContract::new(storage.clone());
    if !contract.liquidity_sources.remove(&source)? {
        return Err(VaultRouterError::LiquiditySourceNotFound.into());
    }
    let source_type = contract.liquidity_source_types.read(&source)?;
    contract.liquidity_source_types.clear(&source)?;

    contract.emit(IVaultRouter::LiquiditySourceRemoved {
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
        return Err(VaultRouterError::ZeroAddress.into());
    }
    if target_type == UNKNOWN {
        return Err(VaultRouterError::InvalidLiquidityTarget.into());
    }

    let mut contract = VaultRouterContract::new(storage.clone());
    contract.liquidity_targets.insert(target)?;
    contract
        .liquidity_target_types
        .write(&target, target_type)?;

    contract.emit(IVaultRouter::LiquidityTargetAdded {
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
    let mut contract = VaultRouterContract::new(storage.clone());
    if !contract.liquidity_targets.remove(&target)? {
        return Err(VaultRouterError::LiquidityTargetNotFound.into());
    }
    let target_type = contract.liquidity_target_types.read(&target)?;
    contract.liquidity_target_types.clear(&target)?;

    contract.emit(IVaultRouter::LiquidityTargetRemoved {
        targetAddress: target,
        targetType: liquidity_target(target_type),
    })
}

// ---------------------------------------------------------------------------
// liquidity flow
// ---------------------------------------------------------------------------

/// Resolves the `StablesSource` registered for `caller`, returning `Unknown`
/// when `caller` is not a registered source.
pub fn registered_liquidity_source(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultRouter::StablesSource> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(liquidity_source(
        contract.liquidity_source_types.read(&caller)?,
    ))
}

/// Resolves the `StablesTarget` registered for `caller`, returning `Unknown`
/// when `caller` is not a registered target.
pub fn registered_liquidity_target(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultRouter::StablesTarget> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(liquidity_target(
        contract.liquidity_target_types.read(&caller)?,
    ))
}

/// `deposit`: pulls `amount` of `asset` from the caller and deposits it
/// into the asset's vault, returning the minted shares.
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

/// `withdraw`: redeems `amount` of `asset` from the vault and tops it
/// up into `receiver` (a token bundle), returning the burned shares.
pub(crate) fn withdraw(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    receiver: Address,
    target: IVaultRouter::StablesTarget,
) -> Result<U256> {
    if receiver.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }
    if matches!(target, IVaultRouter::StablesTarget::Unknown) {
        return Err(VaultRouterError::InvalidLiquidityTarget.into());
    }

    let vault = first_vault(&storage, asset)?;

    let (required_shares, available_shares) = withdraw_shares(&storage, vault, amount)?;
    if available_shares < required_shares {
        return Err(VaultRouterError::InsufficientSharesForWithdraw {
            available: available_shares,
            required: required_shares,
        }
        .into());
    }

    let burned_shares = vault_withdraw(&storage, vault, amount, SELF, SELF)?;

    erc20_approve(&storage, asset, receiver, amount)?;
    token_bundle_top_up(&storage, receiver, SELF, asset, amount)?;

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
// reservations
// ---------------------------------------------------------------------------

/// `reserve`: redeem `amount` of `asset` out of its vault into this router's own
/// custody, held under `id` until released or returned.
///
/// This is the first half of [`withdraw`] with the delivery deferred. It exists
/// because a liquidity *check* cannot survive the gap between quoting a loan and
/// drawing it — another withdrawal can consume the same shares in between. Holding
/// the assets is the only thing that actually guarantees delivery.
pub(crate) fn reserve(
    storage: StorageHandle<'_>,
    id: B256,
    asset: Address,
    amount: U256,
    target: IVaultRouter::StablesTarget,
) -> Result<U256> {
    if matches!(target, IVaultRouter::StablesTarget::Unknown) {
        return Err(VaultRouterError::InvalidLiquidityTarget.into());
    }
    let mut contract = VaultRouterContract::new(storage.clone());
    if !contract.reservation_assets.read(&id)?.is_zero() {
        return Err(VaultRouterError::ReservationExists(id).into());
    }

    let vault = first_vault(&storage, asset)?;
    let (required_shares, available_shares) = withdraw_shares(&storage, vault, amount)?;
    if available_shares < required_shares {
        return Err(VaultRouterError::InsufficientSharesForWithdraw {
            available: available_shares,
            required: required_shares,
        }
        .into());
    }

    // Redeem to SELF: the assets sit with the router, not the eventual receiver,
    // which is not known until release.
    let burned_shares = vault_withdraw(&storage, vault, amount, SELF, SELF)?;

    contract.reservation_assets.write(&id, asset)?;
    contract.reservation_amounts.write(&id, amount)?;

    contract.emit(IVaultRouter::ReservationCreated {
        id,
        asset,
        amount,
        burnedShares: burned_shares,
    })?;

    Ok(burned_shares)
}

/// `releaseReservation`: deliver the assets held under `id` into `receiver` (a
/// token bundle) and delete the reservation. Returns the amount delivered.
pub(crate) fn release_reservation(
    storage: StorageHandle<'_>,
    id: B256,
    receiver: Address,
    target: IVaultRouter::StablesTarget,
) -> Result<U256> {
    if receiver.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }
    if matches!(target, IVaultRouter::StablesTarget::Unknown) {
        return Err(VaultRouterError::InvalidLiquidityTarget.into());
    }
    let (asset, amount) = take_reservation(&storage, id)?;

    erc20_approve(&storage, asset, receiver, amount)?;
    token_bundle_top_up(&storage, receiver, SELF, asset, amount)?;

    let mut contract = VaultRouterContract::new(storage.clone());
    contract.emit(IVaultRouter::ReservationReleased {
        id,
        asset,
        receiver,
        amount,
    })?;

    Ok(amount)
}

/// `returnReservation`: deposit the assets held under `id` back into their vault
/// and delete the reservation. Returns the shares minted back.
///
/// Permissionless by design — putting assets back can harm nobody, and an expiry
/// sweep has no natural privileged caller.
pub(crate) fn return_reservation(storage: StorageHandle<'_>, id: B256) -> Result<U256> {
    let (asset, amount) = take_reservation(&storage, id)?;

    let vault = first_vault(&storage, asset)?;
    // `add_vault` already granted the vault an unlimited allowance on the router,
    // so the deposit needs no fresh approval.
    let minted_shares = vault_deposit(&storage, vault, amount, SELF)?;

    let mut contract = VaultRouterContract::new(storage.clone());
    contract.emit(IVaultRouter::ReservationReturned {
        id,
        asset,
        amount,
        mintedShares: minted_shares,
    })?;

    Ok(minted_shares)
}

/// Reads and deletes the reservation under `id`, rejecting an unknown one. Both
/// consumers delete before their sub-calls, so a re-entrant release can never
/// spend the same reservation twice.
fn take_reservation(storage: &StorageHandle<'_>, id: B256) -> Result<(Address, U256)> {
    let contract = VaultRouterContract::new(storage.clone());
    let asset = contract.reservation_assets.read(&id)?;
    if asset.is_zero() {
        return Err(VaultRouterError::ReservationNotFound(id).into());
    }
    let amount = contract.reservation_amounts.read(&id)?;
    contract.reservation_assets.clear(&id)?;
    contract.reservation_amounts.clear(&id)?;
    Ok((asset, amount))
}

/// `reservationOf`: the asset and amount held under `id`, zeroes when none.
pub fn reservation_of(storage: &StorageHandle<'_>, id: B256) -> Result<(Address, U256)> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok((
        contract.reservation_assets.read(&id)?,
        contract.reservation_amounts.read(&id)?,
    ))
}

// ---------------------------------------------------------------------------
// views
// ---------------------------------------------------------------------------

/// `sharesBalance`: vault shares currently held by this router.
pub fn shares_balance(storage: &StorageHandle<'_>, vault: Address) -> Result<U256> {
    erc20_balance_of(storage, vault, SELF)
}

/// `hasLiquidity`: whether the router holds enough shares to redeem `amount` of
/// `asset`. An asset with no configured vault has no liquidity, which is an
/// answer rather than an error — the caller is asking precisely so it can gate.
pub fn has_liquidity(storage: &StorageHandle<'_>, asset: Address, amount: U256) -> Result<bool> {
    let contract = VaultRouterContract::new(storage.clone());
    let Some(vault) = contract.first_vault(asset)? else {
        return Ok(false);
    };
    let (required_shares, available_shares) = withdraw_shares(storage, vault, amount)?;
    Ok(available_shares >= required_shares)
}

/// Shares `withdraw` would have to burn to redeem `amount`, paired with the
/// shares the router actually holds. Shared with [`has_liquidity`] so the gate
/// and the enforcement can never answer differently.
fn withdraw_shares(
    storage: &StorageHandle<'_>,
    vault: Address,
    amount: U256,
) -> Result<(U256, U256)> {
    Ok((
        vault_preview_withdraw(storage, vault, amount)?,
        erc20_balance_of(storage, vault, SELF)?,
    ))
}

// ---------------------------------------------------------------------------
// helpers: enum reconstruction
// ---------------------------------------------------------------------------

fn liquidity_source(value: u8) -> IVaultRouter::StablesSource {
    IVaultRouter::StablesSource::try_from(value).unwrap_or(IVaultRouter::StablesSource::Unknown)
}

fn liquidity_target(value: u8) -> IVaultRouter::StablesTarget {
    IVaultRouter::StablesTarget::try_from(value).unwrap_or(IVaultRouter::StablesTarget::Unknown)
}

/// Resolves the first vault for `asset`, reverting if none is configured.
fn first_vault(storage: &StorageHandle<'_>, asset: Address) -> Result<Address> {
    let contract = VaultRouterContract::new(storage.clone());
    contract
        .first_vault(asset)?
        .ok_or_else(|| VaultRouterError::ReserveVaultNotConfigured.into())
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
        .map_err(|_| VaultRouterError::UndecodableReturn("ERC20 balanceOf").into())
}

fn vault_asset(storage: &StorageHandle<'_>, vault: Address) -> Result<Address> {
    let ret = storage.staticcall(vault, IVaultV2::assetCall {}.abi_encode().into())?;
    IVaultV2::assetCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 asset").into())
}

fn vault_owner(storage: &StorageHandle<'_>, vault: Address) -> Result<Address> {
    let ret = storage.staticcall(vault, IVaultV2::ownerCall {}.abi_encode().into())?;
    IVaultV2::ownerCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 owner").into())
}

fn asset_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::UndecodableReturn("IReferenceCurrency isoCode").into())
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
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 deposit").into())
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
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 previewWithdraw").into())
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
        .map_err(|_| VaultRouterError::UndecodableReturn("IVaultV2 withdraw").into())
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
        return Err(VaultRouterError::ReceiverNotDeployed.into());
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
