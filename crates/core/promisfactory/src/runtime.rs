//! Orchestration logic for the promisfactory precompile.
//!
//! Owns the promis mint/burn orchestration on top of the Promis token
//! (`outbe_promis::Promis`). `mine` wraps `outbe_promis::Promis::mine`, records
//! the Fidelity acquisition cohort (`cohort_in`), and emits `PromisMined`.
//! `mine_coen` is the symmetric sale path: it wraps `outbe_promis::Promis::burn`,
//! records the Fidelity sale cohort (`cohort_out`), mints native COEN 1:1, and
//! emits `CoenMined`. `convert_to_gratis` burns promis and mints Gratis 1:1 via
//! `gratisfactory::api::mine_from_promis`, deliberately leaving Fidelity untouched
//! so the original acquisition cohort carries over to the gratis. Keeping these
//! here puts the token movement and Fidelity bookkeeping in one place.

use alloy_primitives::{Address, U256};

use crate::precompile::IPromisFactory;
use outbe_primitives::addresses::PROMIS_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::Promis;

/// Mint `amount` promis to `account`, record the Fidelity acquisition cohort,
/// and emit `PromisMined`.
///
/// Internal cross-module API (not exposed on the precompile ABI). The
/// production callers are GemFactory's and IntexFactory's mine paths, which
/// delegate the matching promis mint here. Amount/address validation is
/// delegated to [`outbe_promis::Promis::mine`].
pub fn mine(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    let mut promis = Promis::new(storage.clone());
    promis.mine(account, amount)?;

    let now = storage.timestamp()?.to::<u64>();
    outbe_fidelity::api::cohort_in(storage.clone(), account, amount, now)?;

    storage.emit_event(
        PROMIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IPromisFactory::PromisMined {
            account,
            amount,
        }),
    )?;

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

/// Burn `amount` promis from `account`, emit `PromisBurned`, and mint the matching
/// Gratis 1:1 via [`outbe_gratisfactory::api::mine_from_promis`].
///
/// Unlike [`mine_coen`], this touches no Fidelity cohort: the original promis
/// acquisition cohort stays intact and carries over to the gratis, so loyalty
/// aging is preserved. Amount/balance validation is delegated to
/// [`outbe_promis::Promis::burn`]; atomic revert guarantees no partial burn if the
/// gratis mint fails. Returns the minted gratis amount.
pub fn convert_to_gratis(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
) -> Result<U256> {
    let mut promis = Promis::new(storage.clone());
    promis.burn(account, amount)?;

    storage.emit_event(
        PROMIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IPromisFactory::PromisBurned {
            account,
            amount,
        }),
    )?;

    outbe_gratisfactory::api::mine_from_promis(storage.clone(), account, amount)?;

    Ok(amount)
}
