use super::*;

use outbe_promislimit::{BurnSource, PromisLimitContract};

/// Burns the proceeds into the Promis Limit; COEN below one minor carries in its shared rest.
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
    let credited = PromisLimitContract::new(storage.clone()).burn_to_promis_limit(
        INTEX_FACTORY_ADDRESS,
        amount,
        BurnSource::Proceeds,
    )?;
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
