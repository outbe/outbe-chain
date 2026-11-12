//! Forfeit pass: closes a called group's settlement window, returns the Promis load of
//! everything left unrealized to the Promis Limit, and hands the group to the
//! cleanup.

use alloy_primitives::{B256, U256};
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_primitives::{
    block::BlockRuntimeContext, error::Result, pass_budget::PassBudget, storage::StorageHandle,
};

use outbe_intex::precompile::IIntex;
use outbe_intex::{ForfeitQueue, IntexContract};

use crate::errors::IntexFactoryError;

pub(crate) fn forfeit_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = IntexContract::new(ctx.storage.clone());
    deadline_queue::process_day(
        &ForfeitQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut IntexForfeit::new(ctx),
    )
}

struct IntexForfeit<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    intex: IntexContract<'storage>,
}

impl<'a, 's> IntexForfeit<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            intex: IntexContract::new(ctx.storage.clone()),
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for IntexForfeit<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.intex.group_member_count(group)? {
            0 => Due::Drop,
            _ => Due::Process,
        })
    }

    fn member_count(&self, _group: B256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, group: B256, _index: u32) -> Result<B256> {
        Ok(group)
    }

    /// Returns the group's unrealized load to the pool in one credit.
    fn process_member(&mut self, group: B256, _member: B256) -> Result<()> {
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
            closed_events.push(IIntex::IntexSeriesClosed {
                seriesId: series_id.into(),
                issuanceCurrency: series.issuance_currency,
                referenceCurrency: series.reference_currency,
                forfeitedCount: forfeited.units,
                forfeitedAmountMinor: series_returned,
            });
        }
        if !returned.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .checked_credit(returned)?;
        }
        self.intex.hand_to_cleanup(group)?;
        for event in closed_events {
            self.intex.emit(event)?;
        }
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::intexfactory", %group, retry_at, "forfeit pass: group deferred");
        Ok(())
    }
}
