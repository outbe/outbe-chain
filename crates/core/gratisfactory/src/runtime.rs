//! Ordinary Gratis mint, burn and the COEN conversions.
use crate::{errors::GratisFactoryError, precompile::IGratisFactory};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_fidelity::api::FidelityCohortOp;
use outbe_gratis::api::{self as gratis, ModifyAuth};
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::addresses::GRATIS_FACTORY_ADDRESS;
use outbe_system_common::units::{checked_protocol_to_native, UNITS_PER_PROTOCOL_MINOR};

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort.
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    // Fold the acquisition cohort into the gratis mint round-trip. Persist the
    // returned fidelity blob.
    let now = storage.now_secs()?;
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
    let native_amount =
        checked_protocol_to_native(amount).ok_or(GratisFactoryError::NativeCoenOverflow)?;

    // Fold the sale cohort into the gratis burn round-trip. Persist the returned
    // fidelity blob.
    let now = storage.now_secs()?;
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::Out, now)?;
    let outcome = gratis::burn_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;

    // GRATIS stays at six decimals. The matching native COEN exits at 18 decimals.
    storage.increase_balance(account, native_amount)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        SolEvent::encode_log_data(&IGratisFactory::CoenMinedFromGratis {
            account,
            amountMinor: native_amount,
        }),
    )?;

    Ok(native_amount)
}

/// Burn the native COEN `value` sent to the factory into the Promis Limit and mint
/// the same amount of Gratis, 1:1. `value` must be a non-zero whole number of
/// Gratis minors, so no remainder is left behind.
pub fn mine_gratis(
    storage: StorageHandle<'_>,
    account: Address,
    value: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    if value.is_zero() || !(value % UNITS_PER_PROTOCOL_MINOR).is_zero() {
        return Err(GratisFactoryError::InvalidCoenAmount.into());
    }
    let gratis_minor = value / UNITS_PER_PROTOCOL_MINOR;
    storage.with_checkpoint(|| {
        // The payable route has already credited `value` to this factory.
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .burn(outbe_promislimit::BurnSource::GratisMint, value)?;
        mint(storage.clone(), account, gratis_minor, auth)?;
        storage.emit_event(
            GRATIS_FACTORY_ADDRESS,
            IGratisFactory::GratisMinedFromCoen {
                account,
                amountMinor: gratis_minor,
            }
            .encode_log_data(),
        )
    })?;
    Ok(gratis_minor)
}
