use super::*;

/// Credit auction proceeds (native COEN, arriving as `amount` = msg.value) from
/// one target chain into the day's pot. Gated to the OriginRouter. A ready pot
/// is burned once every winning chain has routed its proceeds, or once the
/// fan-in deadline passes. Proceeds arrive once per winning chain (loopback
/// same-block, remote minutes later). The credit only accumulates. It never
/// reverts on a repeat: a revert there would strand that chain's delivery.
pub fn distribute(
    storage: &StorageHandle<'_>,
    caller: Address,
    worldwide_day: WorldwideDay,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    #[cfg(not(feature = "e2e-test"))]
    let from_router = caller == ORIGIN_ROUTER_ADDRESS;
    #[cfg(feature = "e2e-test")]
    let from_router =
        caller == ORIGIN_ROUTER_ADDRESS || caller == crate::constants::PROCEEDS_TEST_SENDER;
    if !from_router {
        return Err(IntexFactoryError::NotOriginRouter.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }
    outbe_intex::api::credit_proceeds(storage, worldwide_day, src_chain_id, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsCredited {
            worldwideDay: worldwide_day.value(),
            srcChainId: src_chain_id,
            amount,
        },
    )?;
    let now = storage.timestamp()?.to::<u64>();
    try_settle_proceeds(storage, worldwide_day, now)
}

/// Burn a ready pot once fan-in completes or its deadline passes.
/// Repeated arrivals and the begin-block sweep can both call this.
/// `proceeds_deadline == 0` is an early credit before arm and stays put.
/// `proceeds_deadline == u64::MAX` is a day already finalized; a later pot burns.
pub(crate) fn try_settle_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    now: u64,
) -> Result<()> {
    let deadline = outbe_intex::api::proceeds_deadline(storage, worldwide_day)?;
    if deadline == u64::MAX {
        return burn_late_pot(storage, worldwide_day);
    }
    if deadline == 0 {
        return Ok(());
    }
    let complete = outbe_intex::api::proceeds_ready(storage, worldwide_day)?;
    if !complete && now < deadline {
        return Ok(());
    }
    let pot = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
    if !pot.is_zero() {
        burn_ownerless_proceeds(storage, worldwide_day, pot)?;
    }
    outbe_intex::api::finalize_proceeds(storage, worldwide_day)
}

fn burn_late_pot(storage: &StorageHandle<'_>, worldwide_day: WorldwideDay) -> Result<()> {
    let late = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
    if !late.is_zero() {
        burn_late_proceeds(storage, worldwide_day.value(), late)?;
    }
    Ok(())
}

/// Destroy proceeds that arrived after the day was finalized.
fn burn_late_proceeds(storage: &StorageHandle<'_>, worldwide_day: u32, amount: U256) -> Result<()> {
    storage.decrease_balance(INTEX_FACTORY_ADDRESS, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::LateProceedsBurned {
            worldwideDay: worldwide_day,
            amount,
        },
    )
}

/// Settle each WorldwideDay whose proceeds deadline passes.
/// Each day has a separate checkpoint. Propagating failures reject the block.
/// Other failures leave the day for a later sweep.
pub(crate) fn sweep_proceeds_deadlines(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    let count = outbe_intex::api::awaiting_proceeds_count(storage)?;
    // Read the set before settling: settling swap-removes from it.
    let mut worldwide_days = Vec::with_capacity(count as usize);
    for i in 0..count {
        worldwide_days.push(outbe_intex::api::awaiting_proceeds_at(storage, i)?);
    }
    for worldwide_day in worldwide_days {
        let res = storage.with_checkpoint(|| try_settle_proceeds(storage, worldwide_day, now));
        if let Err(e) = res {
            if e.sweep_failure() == SweepFailure::Propagate {
                return Err(e);
            }
            tracing::warn!(target: "outbe::intexfactory", worldwide_day = worldwide_day.value(), error = ?e, "proceeds sweep: skipping series");
        }
    }
    Ok(())
}

/// Burn a ready proceeds pot: destroy the native COEN held by the factory.
fn burn_ownerless_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    amount: U256,
) -> Result<()> {
    storage.decrease_balance(INTEX_FACTORY_ADDRESS, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsBurned {
            worldwideDay: worldwide_day.value(),
            amount,
        },
    )
}
