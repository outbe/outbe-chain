//! Public Solidity ABI of the vaultrouter precompile, plus thin typed helpers
//! that hide the EVM sub-call to `VAULT_ROUTER_ADDRESS`.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{sol, SolCall};

use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

sol!("../../../contracts/precompiles/src/IVaultRouter.sol");

/// `deposit`: deposit `amount` of `asset` into its reserve vault via an
/// EVM sub-call to the vault router. Returns the minted shares.
pub fn deposit(storage: &StorageHandle<'_>, asset: Address, amount: U256) -> Result<U256> {
    let ret = storage.call(
        VAULT_ROUTER_ADDRESS,
        U256::ZERO,
        IVaultRouter::depositCall {
            asset,
            assetsAmount: amount,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultRouter::depositCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("deposit undecodable".into()))
}

/// `referenceCurrencyAssets`: every asset registered under an ISO 4217 code.
/// Read-only, so this uses a staticcall. Order is not stable across removals.
pub fn reference_currency_assets(
    storage: &StorageHandle<'_>,
    iso_code: u16,
) -> Result<Vec<Address>> {
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::referenceCurrencyAssetsCall { isoCode: iso_code }
            .abi_encode()
            .into(),
    )?;
    IVaultRouter::referenceCurrencyAssetsCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("referenceCurrencyAssets undecodable".into()))
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
    let ret = storage.call(
        VAULT_ROUTER_ADDRESS,
        U256::ZERO,
        IVaultRouter::releaseReservationCall {
            id,
            receiver,
            amount,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultRouter::releaseReservationCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("releaseReservation undecodable".into()))
}

/// `returnReservation`: deposit the assets held under `id` back into their vault.
pub fn return_reservation(storage: &StorageHandle<'_>, id: U256) -> Result<U256> {
    let ret = storage.call(
        VAULT_ROUTER_ADDRESS,
        U256::ZERO,
        IVaultRouter::returnReservationCall { id }
            .abi_encode()
            .into(),
    )?;
    IVaultRouter::returnReservationCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("returnReservation undecodable".into()))
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
    let ret = storage.call(
        VAULT_ROUTER_ADDRESS,
        U256::ZERO,
        IVaultRouter::withdrawCall {
            asset,
            amount,
            receiver,
        }
        .abi_encode()
        .into(),
    )?;
    IVaultRouter::withdrawCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("withdraw undecodable".into()))
}
