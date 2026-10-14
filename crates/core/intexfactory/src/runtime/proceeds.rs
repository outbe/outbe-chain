use super::*;

use outbe_primitives::units::native_to_protocol_floor;
use outbe_promislimit::PromisLimitContract;

/// Burn auction proceeds (native COEN, arriving as `amount` = msg.value) from one
/// target chain and credit the same COEN to the Promis limit pool at 1 COEN = 1 Promis.
/// Gated to the OriginRouter. Wei below one protocol unit burns without a credit.
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
        PromisLimitContract::new(storage.clone()).add_to_total_unallocated(credited)?;
    }
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsBurned {
            worldwideDay: worldwide_day.value(),
            srcChainId: src_chain_id,
            amount,
            creditedPromisMinor: credited,
        },
    )
}
