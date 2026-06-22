//! Orchestration logic for the gratisfactory precompile.
//!
//! Bridges the Gratis token (`outbe_gratis::Gratis`) and the shielded pool
//! (`outbe_gratispool`):

use alloy_primitives::{Address, U256};

use crate::errors::GratisFactoryError;
use outbe_gratis::Gratis;
use outbe_gratispool::api as pool;
use outbe_gratispool::SpendArgs;
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

    // Append the commitment first so denomination / duplicate checks fire
    // before any Gratis movement. The pool returns the amount to escrow.
    let (new_root, leaf_index, amount) =
        pool::add_commitment(storage.clone(), denom_id, commitment)?;
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
