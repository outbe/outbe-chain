//! Gratis pledge notes and ordinary mint/burn operations.
use crate::{errors::GratisFactoryError, precompile::IGratisFactory};
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_fidelity::api::FidelityCohortOp;
use outbe_gratis::api::{self as gratis, ModifyAuth};
use outbe_primitives::{
    addresses::GRATIS_FACTORY_ADDRESS,
    error::{PrecompileError, Result},
    storage::StorageHandle,
    units::checked_protocol_to_native,
};

pub fn pledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<B256> {
    storage.with_checkpoint(|| {
        if amount.is_zero() {
            return Err(GratisFactoryError::InvalidAmount.into());
        }
        let now = u64::try_from(storage.timestamp()?)
            .map_err(|_| PrecompileError::Revert("timestamp exceeds u64".into()))?;
        let probe = outbe_fidelity::api::cohort_section(
            storage.clone(),
            caller,
            FidelityCohortOp::Probe,
            now,
        )?;
        let (commitment, outcome) =
            gratis::pledge_with_fidelity(storage.clone(), caller, amount, auth, probe)?;
        // Retain the existing read-only eligibility gate.
        if outcome.league == u16::MAX {
            return Err(GratisFactoryError::FidelityNotEligible.into());
        }
        Ok(commitment)
    })
}
pub fn unpledge_gratis(storage: StorageHandle<'_>, proof: &[u8]) -> Result<U256> {
    gratis::unpledge(storage, proof)
}

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort. The `GratisMinted` event is
/// emitted by the Gratis token.
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

pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let native_amount = checked_protocol_to_native(amount)
        .ok_or_else(|| PrecompileError::Revert("native COEN amount overflow".into()))?;

    // Fold the sale cohort into the gratis burn round-trip; persist the returned
    // fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::Out, now)?;
    let outcome = gratis::burn_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;

    // GRATIS stays at six decimals; the matching native COEN exits at 18 decimals.
    storage.increase_balance(account, native_amount)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        SolEvent::encode_log_data(&IGratisFactory::CoenMined {
            sender: account,
            amount: native_amount,
        }),
    )?;

    Ok(native_amount)
}
