use super::*;
use crate::constants::ORIGIN_ROUTER_ADDRESS;

use outbe_promislimit::{BurnSource, PromisLimitContract};

/// Burns the proceeds into the Promis Limit; COEN below one minor stays as the rest of code 43.
pub fn burn_intex_auction_proceeds(
    storage: &StorageHandle<'_>,
    caller: Address,
    emission_day: EmissionDate,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    if caller != ORIGIN_ROUTER_ADDRESS {
        return Err(IntexFactoryError::NotOriginRouter.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }
    PromisLimitContract::new(storage.clone()).burn(BurnSource::IntexAuctionProceeds, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::IntexAuctionProceedsBurned {
            emissionDay: emission_day.value(),
            srcChainId: src_chain_id,
            amountMinor: amount,
        },
    )
}
