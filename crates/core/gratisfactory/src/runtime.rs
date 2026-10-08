//! Gratis pledges for Credis reservations and ordinary mint/burn operations.
use crate::{
    errors::{CollateralError, GratisFactoryError},
    precompile::IGratisFactory,
    schema::{CollateralAllocation, GratisFactoryContract, PledgeRecord},
};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_fidelity::api::FidelityCohortOp;
use outbe_gratis::api::{self as gratis, ModifyAuth};
use outbe_primitives::{
    addresses::GRATIS_FACTORY_ADDRESS,
    error::{PrecompileError, Result},
    storage::StorageHandle,
    units::checked_protocol_to_native,
};

fn now_secs(storage: &StorageHandle<'_>) -> Result<u64> {
    u64::try_from(storage.timestamp()?)
        .map_err(|_| PrecompileError::Revert("timestamp exceeds u64".into()))
}

/// Pledge exactly the reservation's Gratis from its source. Only the source may
/// pledge, once per reservation, before the reservation expires.
pub fn pledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
    auth: ModifyAuth,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let r = outbe_vaultrouter::api::reservation_of(&storage, reservation_id)?;
        if r.asset.is_zero() {
            return Err(GratisFactoryError::ReservationNotFound.into());
        }
        if caller != r.source {
            return Err(GratisFactoryError::NotReservationSource.into());
        }
        let now = now_secs(&storage)?;
        if now > r.expires_at {
            return Err(GratisFactoryError::ReservationExpired.into());
        }
        let contract = GratisFactoryContract::new(storage.clone());
        if contract.pledges.exists(reservation_id)? {
            return Err(GratisFactoryError::PledgeExists.into());
        }
        let probe = outbe_fidelity::api::cohort_section(
            storage.clone(),
            caller,
            FidelityCohortOp::Probe,
            now,
        )?;
        let outcome =
            gratis::pledge_with_fidelity(storage.clone(), caller, r.gratis_minor, auth, probe)?;
        if outcome.league == u16::MAX {
            return Err(GratisFactoryError::FidelityNotEligible.into());
        }
        contract.pledges.create(&PledgeRecord {
            reservation_id,
            source: caller,
            gratis_minor: r.gratis_minor,
        })?;
        storage.emit_event(
            GRATIS_FACTORY_ADDRESS,
            IGratisFactory::GratisPledged {
                reservationId: reservation_id,
                source: caller,
                gratisMinor: r.gratis_minor,
            }
            .encode_log_data(),
        )
    })
}

/// Return an unused pledge to its source. Only the source may cancel, at any time
/// before Credis uses the pledge.
pub fn cancel_pledge(
    storage: StorageHandle<'_>,
    caller: Address,
    reservation_id: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let pledge = live_pledge(&storage, reservation_id)?;
        if caller != pledge.source {
            return Err(GratisFactoryError::NotReservationSource.into());
        }
        GratisFactoryContract::new(storage.clone())
            .pledges
            .delete(reservation_id)?;
        gratis::release_pledged(&storage, pledge.source, pledge.gratis_minor)?;
        storage.emit_event(
            GRATIS_FACTORY_ADDRESS,
            IGratisFactory::PledgeCancelled {
                reservationId: reservation_id,
                source: pledge.source,
                gratisMinor: pledge.gratis_minor,
            }
            .encode_log_data(),
        )
    })
}

/// Turn the reservation's pledge into the collateral of the Credis position it
/// now backs. The Gratis stays in the source's pledged balance.
pub fn send_to_credis(
    storage: &StorageHandle<'_>,
    reservation_id: U256,
    position_id: U256,
    source: Address,
    gratis_minor: U256,
) -> Result<()> {
    let pledge = live_pledge(storage, reservation_id)?;
    if pledge.source != source || pledge.gratis_minor != gratis_minor {
        return Err(GratisFactoryError::PledgeMismatch.into());
    }
    let contract = GratisFactoryContract::new(storage.clone());
    if contract.collateral.exists(position_id)? {
        return Err(CollateralError::Exists.into());
    }
    contract.pledges.delete(reservation_id)?;
    contract.collateral.create(&CollateralAllocation {
        position_id,
        source,
        remaining_minor: gratis_minor,
    })?;
    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        IGratisFactory::PledgeSentToCredis {
            reservationId: reservation_id,
            positionId: position_id,
        }
        .encode_log_data(),
    )
}

/// Return collateral released by a repayment of `position_id` to its source's
/// liquid balance.
pub fn return_from_credis(
    storage: &StorageHandle<'_>,
    position_id: U256,
    amount: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let source = draw_collateral(storage, position_id, amount)?;
        gratis::release_pledged(storage, source, amount)
    })
}

/// Burn the collateral of a defaulted `position_id` from its source's pledged balance.
pub fn burn_from_credis(
    storage: &StorageHandle<'_>,
    position_id: U256,
    amount: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let source = draw_collateral(storage, position_id, amount)?;
        gratis::burn_pledged(storage, source, amount)
    })
}

/// Take `amount` from the position's collateral and close it at zero. Returns
/// the source whose pledged balance backs it.
fn draw_collateral(
    storage: &StorageHandle<'_>,
    position_id: U256,
    amount: U256,
) -> Result<Address> {
    let contract = GratisFactoryContract::new(storage.clone());
    let mut collateral = contract
        .collateral
        .get(position_id)?
        .ok_or(CollateralError::NotFound)?;
    collateral.remaining_minor = collateral
        .remaining_minor
        .checked_sub(amount)
        .ok_or(CollateralError::Exceeded)?;
    if collateral.remaining_minor.is_zero() {
        contract.collateral.delete(position_id)?;
    } else {
        contract.collateral.update(&collateral)?;
    }
    Ok(collateral.source)
}

/// The collateral backing `position_id`, or zeros once it has closed.
pub fn collateral_of(
    storage: &StorageHandle<'_>,
    position_id: U256,
) -> Result<CollateralAllocation> {
    Ok(GratisFactoryContract::new(storage.clone())
        .collateral
        .get(position_id)?
        .unwrap_or(CollateralAllocation {
            position_id,
            ..Default::default()
        }))
}

/// The unused pledge for `reservation_id`, or zeros when there is none.
pub fn pledge_of(storage: &StorageHandle<'_>, reservation_id: U256) -> Result<PledgeRecord> {
    Ok(GratisFactoryContract::new(storage.clone())
        .pledges
        .get(reservation_id)?
        .unwrap_or(PledgeRecord {
            reservation_id,
            ..Default::default()
        }))
}

fn live_pledge(storage: &StorageHandle<'_>, reservation_id: U256) -> Result<PledgeRecord> {
    GratisFactoryContract::new(storage.clone())
        .pledges
        .get(reservation_id)?
        .ok_or_else(|| GratisFactoryError::PledgeNotFound.into())
}

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort. The Gratis token emits the
/// `GratisMinted` event.
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    // Fold the acquisition cohort into the gratis mint round-trip. Persist the
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

    // Fold the sale cohort into the gratis burn round-trip. Persist the returned
    // fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::Out, now)?;
    let outcome = gratis::burn_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;

    // GRATIS stays at six decimals. The matching native COEN exits at 18 decimals.
    storage.increase_balance(account, native_amount)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        SolEvent::encode_log_data(&IGratisFactory::CoenMined {
            sender: account,
            coenMinor: native_amount,
        }),
    )?;

    Ok(native_amount)
}
