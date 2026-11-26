//! Close pass: closes a called group's settlement window, returns the Promis load of
//! everything left unrealized to the Promis Limit, and hands the group to the
//! cleanup.

use alloy_primitives::{B256, U256};
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};
use outbe_system_common::pass_budget::PassBudget;

use outbe_intex::{CloseQueue, IntexContract};

use crate::errors::IntexFactoryError;

pub(crate) fn close_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = IntexContract::new(ctx.storage.clone());
    deadline_queue::run_through_day(
        &CloseQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut IntexClose::new(ctx),
    )
}

struct IntexClose<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    intex: IntexContract<'storage>,
}

impl<'a, 's> IntexClose<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            intex: IntexContract::new(ctx.storage.clone()),
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for IntexClose<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.intex.group_member_count(group)? {
            0 => Due::Drop,
            _ => Due::Handle,
        })
    }

    fn member_count(&self, _group: B256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, group: B256, _index: u32) -> Result<B256> {
        Ok(group)
    }

    /// Returns the group's unrealized load to the pool in one credit.
    fn handle_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let mut returned = U256::ZERO;
        let mut closed_events = Vec::new();
        for series_id in self.intex.group_members(group)? {
            let series = outbe_intex::api::get_series(self.storage, series_id)?;
            let forfeited = outbe_intex::api::forfeitable_units(self.storage, &series)?;
            let series_returned = forfeited
                .promis_load_minor
                .checked_mul(U256::from(forfeited.units))
                .ok_or(IntexFactoryError::ForfeitedPromisLoadOverflow)?;
            returned = returned
                .checked_add(series_returned)
                .ok_or(IntexFactoryError::ForfeitedPromisLoadOverflow)?;
            closed_events.push(crate::precompile::IIntexFactory::IntexSeriesClosed {
                seriesId: series_id.into(),
                issuanceCurrency: series.issuance_currency,
                referenceCurrency: series.reference_currency,
                forfeitedCount: forfeited.units,
                forfeitedAmountMinor: series_returned,
            });
        }
        if !returned.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .add(outbe_promislimit::LimitSource::Intex, returned)?;
        }
        self.intex.hand_to_cleanup(group)?;
        for event in closed_events {
            crate::runtime::emit_event(self.storage, event)?;
        }
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::intexfactory", %group, retry_at, "close pass: group deferred");
        Ok(())
    }
}
