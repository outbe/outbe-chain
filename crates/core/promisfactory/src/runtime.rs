//! Orchestration logic for the promisfactory precompile.
//!
//! Owns the promis mint/burn orchestration on top of the confidential Promis token
//! (`outbe_promis::api`). Writes are authorized by the caller's Promis modify key
//! (`mac` + `opNonce`). `mint` wraps `outbe_promis::api::mint`; `mine_coen` is the
//! symmetric sale path: it wraps `outbe_promis::api::burn`, mints native COEN 1:1,
//! and emits `CoenMined`.

use alloy_primitives::{Address, U256};

use crate::precompile::IPromisFactory;
use outbe_primitives::addresses::PROMIS_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::api::{self as promis, ModifyAuth};

/// Mint `amount` promis to `account` (authorized by the account owner's modify
/// key). The `PromisMinted` event is emitted by the Promis token.
///
/// Internal cross-module API (not exposed on the precompile ABI). The production
/// callers are GemFactory's and IntexFactory's mine paths, which delegate the
/// matching promis mint here.
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    promis::mint(storage, account, amount, auth)
}

/// Burn `amount` promis from `account`, mint the matching native COEN to `account`
/// 1:1, and emit `CoenMined`. Returns the minted native amount. The confidential
/// burn runs inside the enclave and is authorized by the caller's Promis modify
/// key (`auth`).
pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    promis::burn(storage.clone(), account, amount, auth)?;

    // Mint native COEN to the seller 1:1 against the burned promis.
    storage.increase_balance(account, amount)?;

    storage.emit_event(
        PROMIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IPromisFactory::CoenMined {
            sender: account,
            amount,
        }),
    )?;

    Ok(amount)
}
