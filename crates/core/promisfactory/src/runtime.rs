//! Orchestration logic for the promisfactory precompile.
//!
//! Owns the promis mint/burn orchestration on top of the Promis token
//! (`outbe_promis::Promis`). `mine` wraps `outbe_promis::Promis::mine` and records
//! the Fidelity acquisition cohort (`cohort_in`). `mine_coen` is the symmetric
//! sale path: it wraps `outbe_promis::Promis::burn`, records the Fidelity sale
//! cohort (`cohort_out`), mints native COEN 1:1, and emits `CoenMined`.
//! `convert_to_gratis` burns promis and mints Gratis 1:1 via
//! `gratisfactory::api::mint_from_promis`, deliberately leaving Fidelity untouched
//! so the original acquisition cohort carries over to the gratis. Keeping these
//! here puts the token movement and Fidelity bookkeeping in one place. The
//! `PromisMinted`/`PromisBurned` events are emitted by the Promis token itself.

use alloy_primitives::{Address, U256};

use crate::precompile::IPromisFactory;
use outbe_gratisfactory::api::ModifyAuth;
use outbe_primitives::addresses::PROMIS_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::Promis;

/// Mint `amount` promis to `account` and record the Fidelity acquisition cohort.
/// The `PromisMinted` event is emitted by [`outbe_promis::Promis::mint`].
///
/// Internal cross-module API (not exposed on the precompile ABI). The
/// production callers are GemFactory's and IntexFactory's mine paths, which
/// delegate the matching promis mint here. Amount/address validation is
/// delegated to [`outbe_promis::Promis::mint`].
pub fn mint(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    let mut promis = Promis::new(storage.clone());
    promis.mint(account, amount)?;

    let now = storage.timestamp()?.to::<u64>();
    outbe_fidelity::api::cohort_in(storage, account, amount, now)?;

    Ok(())
}

/// Burn `amount` promis from `account`, record the Fidelity sale cohort (LIFO
/// via `cohort_out`), mint the matching native COEN to `account` 1:1, and emit
/// `CoenMined`. Returns the minted native amount.
///
/// The `mineCoen` precompile entry point delegates here. Amount/balance
/// validation is delegated to [`outbe_promis::Promis::burn`].
pub fn mine_coen(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<U256> {
    let mut promis = Promis::new(storage.clone());
    promis.burn(account, amount)?;

    let now = storage.timestamp()?.to::<u64>();
    outbe_fidelity::api::cohort_out(storage.clone(), account, amount, now)?;

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

/// Burn `amount` promis from `account` and mint the matching Gratis 1:1 via
/// [`outbe_gratisfactory::api::mint_from_promis`]. The `PromisBurned` event is
/// emitted by [`outbe_promis::Promis::burn`].
///
/// Unlike [`mine_coen`], this touches no Fidelity cohort: the original promis
/// acquisition cohort stays intact and carries over to the gratis, so loyalty
/// aging is preserved. Amount/balance validation is delegated to
/// [`outbe_promis::Promis::burn`]; atomic revert guarantees no partial burn if the
/// gratis mint fails. Returns the minted gratis amount.
///
/// The gratis mint is authorized by the account owner's Gratis modify key
/// (`auth`): the confidential mint runs inside the enclave, so the caller must
/// supply a valid `mac`/`opNonce` bound to their current gratis op-nonce.
pub fn convert_to_gratis(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let mut promis = Promis::new(storage.clone());
    promis.burn(account, amount)?;

    outbe_gratisfactory::api::mint_from_promis(storage, account, amount, auth)?;

    Ok(amount)
}
