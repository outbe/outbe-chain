//! Thin typed helpers that hide the EVM sub-call to `VAULT_ROUTER_ADDRESS`.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;

use crate::errors::VaultRouterError;
use crate::precompile::IVaultRouter;

/// `depositFrom`: take `amount` of `asset` directly from `payer` into its reserve vault
/// via an EVM sub-call to the vault router. The calling module must be a registered
/// stablecoin source. Returns the minted shares.
pub fn deposit_from(
    storage: &StorageHandle<'_>,
    payer: Address,
    asset: Address,
    amount: U256,
) -> Result<U256> {
    call_router(
        storage,
        IVaultRouter::depositFromCall {
            payer,
            asset,
            assetsMinor: amount,
        },
    )
}

/// `referenceCurrencyAssets`: the distinct assets of one page of the vaults
/// registered under an ISO 4217 code. `offset` and `limit` index the vault set.
/// Read-only, so this uses a staticcall. Order is not stable across removals.
pub fn reference_currency_assets(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    offset: U256,
    limit: U256,
) -> Result<Vec<Address>> {
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::referenceCurrencyAssetsCall {
            isoCode: iso_code,
            offset,
            limit,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultRouter::referenceCurrencyAssetsCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::RouterCallUndecodable("referenceCurrencyAssets").into())
}

/// `assetVaultsCount`: the number of reserve vaults registered for `asset`.
/// The call does not change state. Thus, it uses a staticcall.
pub fn asset_vaults_count(storage: &StorageHandle<'_>, asset: Address) -> Result<U256> {
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall { asset }
            .abi_encode()
            .into(),
    )?;
    IVaultRouter::assetVaultsCountCall::abi_decode_returns(&ret)
        .map_err(|_| VaultRouterError::RouterCallUndecodable("assetVaultsCount").into())
}

/// Direct storage read of a reservation. Same-process precompiles use this
/// rather than a staticcall so tests sharing the provider can seed and inspect
/// holds without routing through the EVM stub.
pub fn reservation_of(
    storage: &StorageHandle<'_>,
    id: U256,
) -> Result<crate::schema::LiquidityReservation> {
    crate::runtime::reservation_of(storage, id)
}

/// `releaseReservation`: bind `receiver` to the reserved account and pay the recorded
/// CCA via an EVM sub-call. Any unused remainder returns to the origin vault.
pub fn release_reservation(
    storage: &StorageHandle<'_>,
    id: U256,
    receiver: Address,
    amount: U256,
) -> Result<U256> {
    call_router(
        storage,
        IVaultRouter::releaseReservationCall {
            id,
            receiver,
            amountMinor: amount,
        },
    )
}

/// `returnReservation`: deposit the assets held under `id` back into their vault.
pub fn return_reservation(storage: &StorageHandle<'_>, id: U256) -> Result<U256> {
    call_router(storage, IVaultRouter::returnReservationCall { id })
}

/// `withdraw`: redeem `amount` of `asset` from its reserve vault and transfer
/// it to `receiver` via an EVM sub-call to the vault router. Returns the
/// burned shares.
pub fn withdraw(
    storage: &StorageHandle<'_>,
    asset: Address,
    amount: U256,
    receiver: Address,
) -> Result<U256> {
    call_router(
        storage,
        IVaultRouter::withdrawCall {
            asset,
            amountMinor: amount,
            receiver,
        },
    )
}

fn call_router<C: SolCall>(storage: &StorageHandle<'_>, call: C) -> Result<C::Return> {
    let ret = storage.call(VAULT_ROUTER_ADDRESS, U256::ZERO, call.abi_encode().into())?;
    C::abi_decode_returns(&ret).map_err(|_| {
        let name = C::SIGNATURE
            .split_once('(')
            .map_or(C::SIGNATURE, |(name, _)| name);
        VaultRouterError::RouterCallUndecodable(name).into()
    })
}
