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

/// Number of anadosis installments a pledge is spread over. MUST match credis'
/// `NUMBER_OF_ANADOSIS` (the 10-month term) so `payAnadosis` releases exactly
/// `1/N` of the collateral per payment.
const ANADOSIS_INSTALLMENTS: u32 = 10;

/// Pledge `amount` gratis from `caller` into the credis escrow (authorized by the
/// caller's modify key). Returns the confidential pledge handle to present at
/// `requestCredis`.
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
    gratis::pledge(storage, caller, amount, ANADOSIS_INSTALLMENTS, auth)
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
