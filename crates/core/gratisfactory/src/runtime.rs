//! Orchestration logic for the gratisfactory precompile.
//!
//! Bridges the confidential Gratis token (`outbe_gratis::api`) and the Fidelity
//! ledger. `pledge_gratis`/`unpledge_gratis` move gratis into/out of the credis
//! escrow; `mine`/`mine_coen` own the mint/burn plus Fidelity cohort bookkeeping.
//! `mine` wraps `outbe_gratis::api::mine` and records the acquisition cohort
//! (`cohort_in`); `mine_coen` wraps `outbe_gratis::api::burn`, records the sale
//! cohort (`cohort_out`), and mints native COEN 1:1.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;

use crate::errors::GratisFactoryError;
use crate::precompile::IGratisFactory;
use outbe_gratis::api::{self as gratis, ModifyAuth};
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::Promis;

/// Pledge `amount` gratis from `caller` into a pending pledge-lock ticket (authorized
/// by the caller's modify key). Returns the confidential pledge handle to present at
/// `requestCredis`. The anadosis installment count lives on the Credis position, not
/// the pledge.
pub fn pledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<B256> {
    {
        let league = outbe_fidelity::api::league(storage.clone(), caller)?;
        // todo implement correct fidelity check
        if league == u16::MAX {
            return Err(GratisFactoryError::FidelityNotEligible.into());
        }
    }
    gratis::pledge(storage, caller, amount, auth)
}

/// Directly unpledge an unspent pledge back to `caller` (e.g. credis rejected).
pub fn unpledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    pledge_handle: B256,
    auth: ModifyAuth,
) -> Result<()> {
    gratis::unpledge(storage, caller, amount, pledge_handle, auth)
}

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort. The `GratisMinted` event is
/// emitted by the Gratis token; the factory `GratisMined` is emitted here.
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    gratis::mint(storage.clone(), account, amount, auth)?;

    let now = storage.timestamp()?.to::<u64>();
    outbe_fidelity::api::cohort_in(storage.clone(), account, amount, now)?;

    Ok(())
}

/// Mint `amount` gratis to `account` WITHOUT recording a Fidelity acquisition
/// cohort (unlike [`mint`]). Used by the promis→gratis conversion, where the
/// original promis acquisition cohort is preserved so loyalty aging carries over.
/// Authorized by the account owner's modify key; the `GratisMinted` event is
/// emitted by [`outbe_gratis::api::mint`].
pub fn mint_from_promis(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    gratis::mint(storage, account, amount, auth)
}

/// Burn `amount` public promis from `account` and mint the matching Gratis 1:1.
/// The `PromisBurned` event is emitted by [`outbe_promis::Promis::burn`].
///
/// Unlike [`mine_coen`], this touches no Fidelity cohort: the original promis
/// acquisition cohort stays intact and carries over to the gratis, so loyalty
/// aging is preserved (the mint goes through [`mint_from_promis`], the no-cohort
/// path). Amount/balance validation is delegated to [`outbe_promis::Promis::burn`];
/// atomic revert guarantees no partial burn if the gratis mint fails. Returns the
/// minted gratis amount.
///
/// The gratis mint is authorized by the account owner's Gratis modify key
/// (`auth`): the confidential mint runs inside the enclave, so the caller must
/// supply a valid `mac`/`opNonce` bound to their current gratis op-nonce.
pub fn mine_from_promis(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let mut promis = Promis::new(storage.clone());
    promis.burn(account, amount)?;

    mint_from_promis(storage, account, amount, auth)?;

    Ok(amount)
}

pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    gratis::burn(storage.clone(), account, amount, auth)?;

    let now = storage.timestamp()?.to::<u64>();
    outbe_fidelity::api::cohort_out(storage.clone(), account, amount, now)?;

    // Mint native COEN to the seller 1:1 against the burned gratis.
    storage.increase_balance(account, amount)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        SolEvent::encode_log_data(&IGratisFactory::CoenMined {
            sender: account,
            amount,
        }),
    )?;

    Ok(amount)
}
