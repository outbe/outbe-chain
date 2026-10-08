//! Expiry sweep: closes a called group's settlement window and returns the Promis
//! load of everything left unrealized to the unallocated limit.

use alloy_primitives::U256;
use outbe_intex::SeriesId;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    expiry_queue::{self, Due, ExpiryHandler},
    storage::StorageHandle,
    sweep_budget::SweepBudget,
};

use crate::constants::MAX_SERIES_ACTIONS_PER_BLOCK;
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;
use crate::state::ExpiryHours;

/// Retire every group whose settlement window has closed, earliest bucket first.
pub(crate) fn sweep_expiry_deadlines(ctx: &BlockRuntimeContext) -> Result<()> {
    let queue = IntexFactoryContract::new(ctx.storage.clone());
    let mut expiry = IntexExpiry {
        storage: &ctx.storage,
        factory: IntexFactoryContract::new(ctx.storage.clone()),
    };
    let mut budget = SweepBudget::new(
        MAX_SERIES_ACTIONS_PER_BLOCK,
        MAX_SERIES_ACTIONS_PER_BLOCK,
        0,
    );
    expiry_queue::sweep(
        &ExpiryHours(&queue),
        &ctx.storage,
        ctx.block.timestamp,
        &mut budget,
        &mut expiry,
    )
}

struct IntexExpiry<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    factory: IntexFactoryContract<'storage>,
}

/// A called group's members are its series, by their place in the group.
impl ExpiryHandler<u64> for IntexExpiry<'_, '_> {
    type Member = (u32, SeriesId);

    fn due(&mut self, key: u64) -> Result<Due> {
        Ok(match self.factory.called_group_count.read(&key)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, key: u64) -> Result<u32> {
        self.factory.called_group_count.read(&key)
    }

    fn member_at(&self, key: u64, index: u32) -> Result<(u32, SeriesId)> {
        let (iso_code, worldwide_day) = IntexFactoryContract::unscoped(key);
        Ok((
            index,
            self.factory.called_member(iso_code, worldwide_day, index)?,
        ))
    }

    /// Returns the series' unrealized load to the pool and takes it out of the group.
    fn expire_member(&mut self, key: u64, (index, series_id): (u32, SeriesId)) -> Result<()> {
        let forfeited = outbe_intex::api::expire_series(self.storage, series_id)?;
        let returned = forfeited
            .promis_load_minor
            .checked_mul(U256::from(forfeited.units))
            .ok_or_else(|| PrecompileError::Revert("forfeited promis load overflow".into()))?;
        emit_event(
            self.storage,
            crate::precompile::IIntexFactory::SeriesExpired {
                seriesId: series_id.into(),
                forfeitedUnits: forfeited.units,
                returnedPromisMinor: returned,
            },
        )?;
        if !returned.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .add_to_total_unallocated(returned)?;
        }
        let (iso_code, worldwide_day) = IntexFactoryContract::unscoped(key);
        self.factory
            .remove_called_member(iso_code, worldwide_day, index)
    }

    fn deferred(&mut self, key: u64, retry_at: u64) -> Result<()> {
        let (iso_code, worldwide_day) = IntexFactoryContract::unscoped(key);
        tracing::warn!(
            target: "outbe::intexfactory",
            iso_code,
            worldwide_day = worldwide_day.value(),
            retry_at,
            "expiry sweep: group deferred"
        );
        emit_event(
            self.storage,
            crate::precompile::IIntexFactory::ExpiryDeferred {
                referenceCurrency: iso_code,
                worldwideDay: worldwide_day.value(),
                retryAt: retry_at,
            },
        )
    }
}
