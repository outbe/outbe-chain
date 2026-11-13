//! Forfeit pass: forfeits the called groups whose notice period lapsed, one credit per
//! group, and hands their unpaid Nods to the cleanup.

use alloy_primitives::B256;
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, PrecompileError, Result},
    storage::StorageHandle,
};

use outbe_system_common::pass_budget::PassBudget;

use outbe_nod::state::ForfeitQueue;
use outbe_nod::{precompile::INod, schema::NodContract};

pub(crate) fn forfeit_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = NodContract::new(ctx.storage.clone());
    deadline_queue::process_day(
        &ForfeitQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut NodForfeit::new(ctx),
    )
}

struct NodForfeit<'a, 's> {
    storage: &'a StorageHandle<'s>,
    nod: NodContract<'s>,
}

impl<'a, 's> NodForfeit<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            nod: NodContract::new(ctx.storage.clone()),
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for NodForfeit<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.nod.group_called_at.read(&group)? != 0;
        Ok(match called && !self.nod.group_is_forfeited.read(&group)? {
            true => Due::Process,
            false => Due::Drop,
        })
    }

    fn member_count(&self, _group: B256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, group: B256, _index: u32) -> Result<B256> {
        Ok(group)
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    /// Returns the Gratis load of the group's unpaid Nods and unclaimed Tributes in one credit.
    /// Lysis drew it out of the day limit and only mining converts it, so unmined it goes back.
    fn process_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let load = self.nod.read_group_open_load(group)?;
        let closed_series = self.collect_closed_series(group)?;
        if !load.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .checked_credit(load)?;
        }
        self.nod.group_open_load_ct.bytes(&group).clear()?;
        for &(series_id, ..) in &closed_series {
            let gs_key = outbe_nod::index_keys::group_series_key(group, series_id);
            self.nod.group_series_open_load_ct.bytes(&gs_key).clear()?;
        }
        let unpaid = self.nod.group_nod_count.read(&group)?;
        let unclaimed = self.nod.group_unclaimed_tribute_count.read(&group)?;
        if unpaid == 0 && unclaimed == 0 {
            // Nothing left to burn. The close event still fires, then the group leaves
            // the forfeit queue without a cleanup entry.
            self.nod.remove_called(group)?;
            let settled = self
                .nod
                .groups
                .get(group)?
                .map(|body| body.settled_nods)
                .unwrap_or(0);
            if settled == 0 {
                self.nod.close_group_if_empty_after_forfeit(group)?;
            } else {
                // Forfeit has run. Later removal of the settled Nods may close the group.
                self.nod.group_is_forfeited.write(&group, true)?;
            }
        } else {
            self.nod.group_is_forfeited.write(&group, true)?;
            self.nod.hand_to_cleanup(group)?;
        }
        for (
            series_id,
            issuance_currency,
            reference_currency,
            forfeited_count,
            forfeited_tribute_count,
            forfeited_amount_minor,
        ) in closed_series
        {
            self.nod.emit(INod::NodSeriesClosed {
                seriesId: series_id.into(),
                issuanceCurrency: issuance_currency,
                referenceCurrency: reference_currency,
                forfeitedCount: forfeited_count,
                forfeitedTributeCount: forfeited_tribute_count,
                forfeitedAmountMinor: forfeited_amount_minor,
            })?;
        }
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::nodfactory", %group, retry_at, "forfeit pass: group deferred");
        Ok(())
    }
}

type ClosedSeriesEntry = (
    outbe_primitives::series_id::SeriesId,
    u16,
    u16,
    u32,
    u32,
    alloy_primitives::U256,
);

impl NodForfeit<'_, '_> {
    fn collect_closed_series(&self, group: B256) -> Result<Vec<ClosedSeriesEntry>> {
        let series_list = self.nod.group_series.list(&group).read_all()?;
        let mut closed_series = Vec::with_capacity(series_list.len());
        for series_id in series_list {
            let gs_key = outbe_nod::index_keys::group_series_key(group, series_id);
            let forfeited_count = self.nod.group_series_nod_count.read(&gs_key)?;
            let unclaimed_tributes = self.nod.series_unclaimed_tribute_count.read(&series_id)?;
            let forfeited_tribute_count = u32::try_from(unclaimed_tributes).map_err(|_| {
                outbe_nod::errors::NodError::GroupMemberIndexOverflow { group_key: group }
            })?;
            let forfeited_amount_minor = self.nod.read_series_open_load(group, series_id)?;
            let issuance_currency = self.nod.series_issuance_currency.read(&series_id)?;
            let reference_currency = self.nod.series_reference_currency.read(&series_id)?;
            closed_series.push((
                series_id,
                issuance_currency,
                reference_currency,
                forfeited_count,
                forfeited_tribute_count,
                forfeited_amount_minor,
            ));
        }
        Ok(closed_series)
    }
}
