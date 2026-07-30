//! Orchestration logic for the gratisfactory precompile.
//!
//! Bridges the confidential Gratis token (`outbe_gratis::api`) and the Fidelity
//! ledger. `pledge_gratis`/`unpledge_gratis` move gratis into/out of the credis
//! escrow; `mine`/`mine_coen` own the mint/burn plus Fidelity cohort bookkeeping.
//! The Fidelity cohort op rides INSIDE the gratis enclave round-trip (no extra
//! trip): `mine` folds an acquisition (`In`), `mine_coen` a sale (`Out`), and
//! `pledge_gratis` a read-only league `Probe` for the eligibility gate. The
//! factory persists the returned fidelity outcome. `mine_from_promis` burns
//! public promis and reuses `mine` (promis itself is fidelity-neutral).

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;

use crate::errors::GratisFactoryError;
use crate::precompile::IGratisFactory;
use outbe_fidelity::api::FidelityCohortOp;
use outbe_gratis::api::{self as gratis, ModifyAuth};
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

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
    // Fold a read-only league probe into the pledge round-trip (no separate
    // fidelity call): the pledge op returns the caller's current league.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), caller, FidelityCohortOp::Probe, now)?;
    let (handle, outcome) = gratis::pledge_with_fidelity(storage, caller, amount, auth, section)?;
    // todo implement correct fidelity eligibility check on `outcome.league`
    if outcome.league == u16::MAX {
        return Err(GratisFactoryError::FidelityNotEligible.into());
    }
    Ok(handle)
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
    // Fold the acquisition cohort into the gratis mint round-trip; persist the
    // returned fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::In, now)?;
    let outcome = gratis::mint_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;
    Ok(())
}

/// Burn `amount` confidential promis from `account` and mint the matching Gratis
/// 1:1. Both tokens are enclave-confidential and independently authorized: the
/// promis burn takes the account owner's **Promis** modify key (`promis_auth`) and
/// the gratis mint takes their **Gratis** modify key (`gratis_auth`). Each `auth`
/// binds `amount` to that ledger's own current op-nonce, so the caller supplies two
/// `mac`/`opNonce` pairs.
pub fn mine_from_promis(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    gratis_auth: ModifyAuth,
    promis_auth: ModifyAuth,
) -> Result<U256> {
    outbe_promis::api::burn(storage.clone(), account, amount, promis_auth)?;

    // Reuse `mint`: gratis mint + fresh Fidelity cohort at the current block time.
    mint(storage, account, amount, gratis_auth)?;

    Ok(amount)
}

pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    // Fold the sale cohort into the gratis burn round-trip; persist the returned
    // fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::Out, now)?;
    let outcome = gratis::burn_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;

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
