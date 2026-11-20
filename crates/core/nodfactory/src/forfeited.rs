//! Forfeit pass: forfeits the called groups whose notice period lapsed, one credit per
//! group, and hands their unpaid Nods to the cleanup.

use alloy_primitives::B256;
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_core_common::series_id::SeriesId;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, PrecompileError, Result},
    storage::StorageHandle,
};

use outbe_system_common::pass_budget::{PassBudget, WITNESS_BYTES_PER_WRITE};

use outbe_nod::errors::NodError;
use outbe_nod::index_keys::group_series_key;
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

const GROUP_LOAD_READ_COST: u32 =
    outbe_nod::api::GROUP_LOAD_READ_WITNESS_BYTES.div_ceil(WITNESS_BYTES_PER_WRITE);

/// One series of a called group, in list order; the last one closes the group.
#[derive(Clone, Copy)]
struct SeriesSlot {
    position: u32,
    last: bool,
}

/// A called group forfeits series by series, one enclave read each, so a large group spans blocks.
impl DeadlineHandler<B256> for NodForfeit<'_, '_> {
    type Member = SeriesSlot;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.nod.group_called_at.read(&group)? != 0;
        Ok(match called && !self.nod.group_is_forfeited.read(&group)? {
            true => Due::Process,
            false => Due::Drop,
        })
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        Ok(self.nod.group_series.list(&group).len()?.max(1))
    }

    fn member_at(&self, group: B256, index: u32) -> Result<SeriesSlot> {
        let count = self.member_count(group)?;
        Ok(SeriesSlot {
            position: count - 1 - index,
            last: index == 0,
        })
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    fn member_cost(&self, _group: B256, _slot: SeriesSlot) -> u32 {
        GROUP_LOAD_READ_COST
    }

    fn process_member(&mut self, group: B256, slot: SeriesSlot) -> Result<()> {
        self.nod.group_forfeiting.write(&group, true)?;
        if let Some(series_id) = self.nod.group_series.list(&group).get(slot.position)? {
            self.close_series(group, series_id)?;
        }
        if slot.last {
            self.close_group(group)?;
        }
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::nodfactory", %group, retry_at, "forfeit pass: group deferred");
        Ok(())
    }
}

impl NodForfeit<'_, '_> {
    /// Adds the series' open load to the group's credit and emits its close, once per series.
    fn close_series(&mut self, group: B256, series_id: SeriesId) -> Result<()> {
        let gs_key = group_series_key(group, series_id);
        if self.nod.group_series_forfeited.read(&gs_key)? {
            return Ok(());
        }
        let forfeited_count = self.nod.group_series_nod_count.read(&gs_key)?;
        let forfeited_tribute_count =
            u32::try_from(self.nod.series_unclaimed_tribute_count.read(&series_id)?)
                .map_err(|_| NodError::GroupMemberIndexOverflow { group_key: group })?;
        let forfeited_amount_minor = self.nod.read_series_open_load(group, series_id)?;
        let total = self
            .nod
            .group_forfeit_load_minor
            .read(&group)?
            .checked_add(forfeited_amount_minor)
            .ok_or(NodError::GroupUnpaidLoadOverflow { group_key: group })?;
        self.nod.group_forfeit_load_minor.write(&group, total)?;
        self.nod.group_series_open_load_ct.bytes(&gs_key).clear()?;
        self.nod.group_series_forfeited.write(&gs_key, true)?;
        self.nod.emit(INod::NodSeriesClosed {
            seriesId: series_id.into(),
            issuanceCurrency: self.nod.series_issuance_currency.read(&series_id)?,
            referenceCurrency: self.nod.series_reference_currency.read(&series_id)?,
            forfeitedCount: forfeited_count,
            forfeitedTributeCount: forfeited_tribute_count,
            forfeitedAmountMinor: forfeited_amount_minor,
        })
    }

    /// Returns the Gratis load of the group's unpaid Nods and unclaimed Tributes in one credit.
    /// Lysis drew it out of the day limit and only mining converts it, so unmined it goes back.
    fn close_group(&mut self, group: B256) -> Result<()> {
        // A series a failed slot left open closes on the retry, one read per slot.
        for series_id in self.nod.group_series.list(&group).read_all()? {
            let gs_key = group_series_key(group, series_id);
            if !self.nod.group_series_forfeited.read(&gs_key)? {
                return Err(PrecompileError::Revert(
                    "forfeit pass: a series of the group is still open".into(),
                ));
            }
        }
        let load = self.nod.group_forfeit_load_minor.read(&group)?;
        if !load.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .add(outbe_promislimit::LimitSource::Nod, load)?;
        }
        self.nod.group_forfeit_load_minor.clear(&group)?;
        self.nod.group_forfeiting.clear(&group)?;
        self.nod.group_open_load_ct.bytes(&group).clear()?;
        let unpaid = self.nod.group_nod_count.read(&group)?;
        let unclaimed = self.nod.group_unclaimed_tribute_count.read(&group)?;
        if unpaid == 0 && unclaimed == 0 {
            // Nothing left to burn. The close events have fired, so the group leaves
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
        Ok(())
    }
}
