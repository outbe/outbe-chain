//! Forfeit pass: closes a called group's settlement window, returns the Promis load of
//! everything left unrealized to the Promis Limit, and hands the group to the
//! cleanup.

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    deadline_queue::{self, Due, DeadlineHandler},
    storage::StorageHandle,
    pass_budget::PassBudget,
};

use outbe_intex::precompile::IIntex;
use outbe_intex::{ForfeitQueue, IntexContract};

use crate::errors::IntexFactoryError;

/// This function forfeits every group after its settlement window closes, earliest day first.
/// It returns the number of forfeited groups.
#[cfg(test)]
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = IntexContract::new(ctx.storage.clone());
    let mut expiry = IntexForfeit::new(ctx);
    deadline_queue::process_block(&ForfeitQueue(&queue), ctx, &mut expiry)?;
    Ok(expiry.expired)
}

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
    expired: u32,
}

impl<'a, 's> IntexForfeit<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            intex: IntexContract::new(ctx.storage.clone()),
            expired: 0,
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
        let mut forfeited_units: u64 = 0;
        let mut returned = U256::ZERO;
        for series_id in self.intex.group_members(group)? {
            let series = outbe_intex::api::read_series(self.storage, series_id)?;
            let forfeited = outbe_intex::api::forfeitable_units(self.storage, &series)?;
            forfeited_units = forfeited_units.saturating_add(u64::from(forfeited.units));
            returned = forfeited
                .promis_load_minor
                .checked_mul(U256::from(forfeited.units))
                .and_then(|load| returned.checked_add(load))
                .ok_or(IntexFactoryError::ForfeitedPromisLoadOverflow)?;
        }
        if !returned.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .checked_credit(returned)?;
        }
        self.intex.hand_to_cleanup(group)?;
        self.intex.emit(IIntex::IntexGroupForfeited {
            groupKey: group,
            forfeitedUnits: forfeited_units,
            returnedPromisMinor: returned,
        })?;
        self.expired = self.expired.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::intexfactory", %group, retry_at, "forfeit pass: group deferred");
        Ok(())
    }
}
