use super::*;

use outbe_primitives::units::native_to_protocol_floor;
use outbe_promislimit::PromisLimitContract;

/// Burns the proceeds and credits whole units to the Promis limit pool; sub-unit wei is not credited.
pub fn distribute(
    storage: &StorageHandle<'_>,
    caller: Address,
    worldwide_day: WorldwideDay,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    if caller != ORIGIN_ROUTER_ADDRESS {
        return Err(IntexFactoryError::NotOriginRouter.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }
    storage.decrease_balance(INTEX_FACTORY_ADDRESS, amount)?;
    let credited = native_to_protocol_floor(amount);
    if !credited.is_zero() {
        PromisLimitContract::new(storage.clone()).checked_credit(credited)?;
    }
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsBurned {
            worldwideDay: worldwide_day.value(),
            srcChainId: src_chain_id,
            proceedsMinor: amount,
            creditedPromisMinor: credited,
        },
    )
}
