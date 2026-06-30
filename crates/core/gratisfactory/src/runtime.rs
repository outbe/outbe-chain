//! Orchestration logic for the gratisfactory precompile.
//!
//! Bridges the Gratis token (`outbe_gratis::Gratis`) and the shielded pool
//! (`outbe_gratispool`):
//!
//! Also owns the gratis mint/burn orchestration. `mine` wraps
//! `outbe_gratis::Gratis::mine`, records the Fidelity acquisition cohort
//! (`cohort_in`), and emits `GratisMined`. `mine_coen` is the symmetric
//! sale path: it wraps `outbe_gratis::Gratis::burn`, records the Fidelity
//! sale cohort (`cohort_out`), mints native COEN 1:1, and emits `CoenMined`.
//! Keeping both here puts the token movement and Fidelity bookkeeping in one
//! place.

use alloy_primitives::{Address, U256};

use crate::errors::GratisFactoryError;
use crate::precompile::IGratisFactory;
use outbe_gratis::Gratis;
use outbe_gratispool::api as pool;
use outbe_gratispool::constants::DenomAmount;
use outbe_gratispool::SpendArgs;
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Append a user-supplied pledge commitment to the pool and move the
/// caller's Gratis into the credis escrow.
/// Returns `(new_root, leaf_index, amount)` for caller logging convenience.
pub fn pledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    denom_id: u8,
    commitment: U256,
) -> Result<(U256, u32, U256)> {
    {
        let league = outbe_fidelity::api::league(storage.clone(), caller)?;
        // todo implement correct fidelity check
        if league == u16::MAX {
            return Err(GratisFactoryError::FidelityNotEligible.into());
        }
    }

    let denom = DenomAmount::try_from(denom_id)?;
    // Reserved sub-rungs (e.g. the anadosis-only `Gratis0_1`) exist solely as
    // reclaim-note destinations and must never accept a direct user pledge.
    if !denom.is_pledgeable() {
        return Err(GratisFactoryError::DenomNotPledgeable.into());
    }
    let (new_root, leaf_index, amount) = pool::add_commitment(storage.clone(), denom, commitment)?;
    let mut gratis = Gratis::new(storage);
    gratis.pledge(caller, amount)?;
    Ok((new_root, leaf_index, amount))
}

/// Verify an unpledge spend proof, consume the nullifier, and release the
/// denomination amount of Gratis from the credis escrow back to `caller`.
/// The proof's `receiver_binding` must bind to `caller`; the per-pledger
/// ledger is keyed by depositor, so `caller` must also be the original
/// pledger.
pub fn unpledge_gratis(
    storage: StorageHandle<'_>,
    args: &SpendArgs,
    caller: Address,
) -> Result<U256> {
    let amount = pool::verify_and_spend_for_unpledge(storage.clone(), caller, args)?;
    let mut gratis = Gratis::new(storage);
    gratis.unpledge(caller, amount)?;
    Ok(amount)
}

pub fn mine(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    let mut gratis = Gratis::new(storage.clone());
    gratis.mine(account, amount)?;

    let now = storage.timestamp()?.to::<u64>();
    outbe_fidelity::api::cohort_in(storage.clone(), account, amount, now)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IGratisFactory::GratisMined {
            account,
            amount,
        }),
    )?;

    Ok(())
}

pub fn mine_coen(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<U256> {
    let mut gratis = Gratis::new(storage.clone());
    gratis.burn(account, amount)?;

    let now = storage.timestamp()?.to::<u64>();
    outbe_fidelity::api::cohort_out(storage.clone(), account, amount, now)?;

    // Mint native COEN to the seller 1:1 against the burned gratis.
    storage.increase_balance(account, amount)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IGratisFactory::CoenMined {
            sender: account,
            amount,
        }),
    )?;

    Ok(amount)
}
