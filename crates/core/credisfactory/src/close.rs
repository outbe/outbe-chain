//! Closes the called groups whose settlement window lapsed, earliest deadline first,
//! and hands their Credis to the cleanup.

use alloy_primitives::B256;
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_credis::{CloseQueue, CredisContract};

use crate::called::emit;
use crate::precompile::ICredisFactory;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
use outbe_system_common::pass_budget::PassBudget;

pub(crate) fn close_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = CredisContract::new(ctx.storage.clone());
    deadline_queue::run_through_day(
        &CloseQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut CredisClose::new(ctx),
    )
}

struct CredisClose<'storage> {
    credis: CredisContract<'storage>,
}

impl<'s> CredisClose<'s> {
    fn new(ctx: &BlockRuntimeContext<'s>) -> Self {
        Self {
            credis: CredisContract::new(ctx.storage.clone()),
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for CredisClose<'_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        // A called group closes even after every Credis has already settled.
        if self.credis.group_called_at.read(&group)? != 0 {
            return Ok(Due::Handle);
        }
        Ok(Due::Drop)
    }

    fn member_count(&self, _group: B256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, group: B256, _index: u32) -> Result<B256> {
        Ok(group)
    }

    /// Forfeits the group's Credis at once. Each one's Gratis burns and returns to the
    /// Promis reserve together, in the cleanup.
    fn handle_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let sub_series = self.credis.group_sub_series(group)?;
        let reference_currency = self.credis.group_currency.read(&group)?;
        if self.credis.group_member_count(group)? == 0 {
            self.credis.remove_called(group)?;
        } else {
            self.credis.hand_to_cleanup(group)?;
        }
        for sub in sub_series {
            emit(
                &self.credis.storage,
                &ICredisFactory::CredisSeriesClosed {
                    seriesId: sub.series_id.into(),
                    subSeries: sub.sub_series,
                    issuanceCurrency: sub.issuance_currency,
                    referenceCurrency: reference_currency,
                    forfeitedCount: sub.unsettled_count,
                    forfeitedAmountMinor: sub.outstanding_amount_minor,
                },
            )?;
        }
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %group, retry_at, "close pass: group deferred");
        Ok(())
    }
}
