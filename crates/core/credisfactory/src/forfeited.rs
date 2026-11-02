//! Forfeits the called groups whose settlement window lapsed, earliest deadline first,
//! and hands their Credis to the cleanup.

use outbe_credis::precompile::ICredis;
use outbe_credis::{CredisContract, ForfeitQueue};
use outbe_primitives::{
    block::BlockRuntimeContext,
    deadline_queue::{self, DeadlineHandler, Due},
    error::Result,
    pass_budget::PassBudget,
    series_id::SeriesId,
};

pub(crate) fn forfeit_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = CredisContract::new(ctx.storage.clone());
    deadline_queue::process_day(
        &ForfeitQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut CredisForfeit::new(ctx),
    )
}

struct CredisForfeit<'storage> {
    credis: CredisContract<'storage>,
}

impl<'s> CredisForfeit<'s> {
    fn new(ctx: &BlockRuntimeContext<'s>) -> Self {
        Self {
            credis: CredisContract::new(ctx.storage.clone()),
        }
    }
}

/// A called series forfeits as one: it is its own and only member.
impl DeadlineHandler<SeriesId> for CredisForfeit<'_> {
    type Member = SeriesId;

    fn due(&mut self, series: SeriesId) -> Result<Due> {
        let called = self.credis.series_called_at.read(&series)? != 0;
        Ok(
            match called && self.credis.series_member_count(series)? != 0 {
                true => Due::Process,
                false => Due::Drop,
            },
        )
    }

    fn member_count(&self, _series: SeriesId) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, series: SeriesId, _index: u32) -> Result<SeriesId> {
        Ok(series)
    }

    /// Forfeits the series's Credis at once. Each one's Gratis burns and returns to the
    /// Promis reserve together, in the cleanup.
    fn process_member(&mut self, series: SeriesId, _member: SeriesId) -> Result<()> {
        let forfeited_count = self.credis.series_member_count(series)?;
        let forfeited_amount_minor = self.credis.series_unpaid_gratis_minor.read(&series)?;
        let issuance_currency = self.credis.series_issuance_currency.read(&series)?;
        let reference_currency = self.credis.series_currency.read(&series)?;
        self.credis.hand_to_cleanup(series)?;
        self.credis.emit(ICredis::CredisSeriesClosed {
            seriesId: series.into(),
            issuanceCurrency: issuance_currency,
            referenceCurrency: reference_currency,
            forfeitedCount: forfeited_count,
            forfeitedAmountMinor: forfeited_amount_minor,
        })?;
        Ok(())
    }

    fn deferred(&mut self, series: SeriesId, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %series, retry_at, "forfeit pass: series deferred");
        Ok(())
    }
}
