//! Orchestration logic for the promisfactory precompile.
//!
//! Owns the promis mint/burn orchestration on top of the confidential Promis token
//! (`outbe_promis::api`). Writes are authorized by the caller's Promis modify key
//! (`mac` + `opNonce`). `mint` wraps `outbe_promis::api::mint`. `mine_coen` is the
//! symmetric sale path: it wraps `outbe_promis::api::burn`, mints native COEN 1:1,
//! and emits `CoenMined`.

use alloy_primitives::{Address, U256};

use crate::errors::PromisFactoryError;
use crate::precompile::IPromisFactory;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::api::{self as promis, ModifyAuth};
use outbe_system_common::addresses::PROMIS_FACTORY_ADDRESS;
use outbe_system_common::units::checked_protocol_to_native;

/// Mint `amount` promis to `account` from a settled Gem position (`gem_id`).
///
/// Internal cross-module API (not exposed on the precompile ABI).
pub fn mint_from_gem(
    storage: StorageHandle<'_>,
    gem_id: U256,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    promis::mint_from_gem(storage, gem_id, account, amount, auth)
}

/// Mint `amount` promis to `account` from a settled Intex position (`intex_id`).
///
/// Internal cross-module API (not exposed on the precompile ABI).
pub fn mint_from_intex(
    storage: StorageHandle<'_>,
    intex_id: U256,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    promis::mint_from_intex(storage, intex_id, account, amount, auth)
}

/// Burn `amount` promis from `account`, mint the matching native COEN to `account`
/// 1:1, and emit `CoenMinedFromPromis`. Returns the minted native amount. The
/// confidential burn runs inside the enclave. The caller's Promis modify key
/// (`auth`) authorizes the burn.
pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let native_amount =
        checked_protocol_to_native(amount).ok_or(PromisFactoryError::NativeCoenOverflow)?;

    promis::burn(storage.clone(), account, amount, auth)?;

    // PROMIS stays at six decimals. The matching native COEN exits at 18 decimals.
    storage.increase_balance(account, native_amount)?;

    storage.emit_event(
        PROMIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IPromisFactory::CoenMinedFromPromis {
            account,
            amountMinor: native_amount,
        }),
    )?;

    Ok(native_amount)
}
