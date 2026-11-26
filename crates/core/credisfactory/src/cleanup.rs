//! Cleanup sweep: deletes the Credis of the forfeited groups. Each one takes its CCA's
//! default, and its Gratis burns from the source's pledge into the Promis reserve.

use alloy_primitives::{B256, U256};
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_credis::{CleanupQueue, CredisContract};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, PrecompileError, Result},
};
use outbe_system_common::pass_budget::{PassBudget, WITNESS_BYTES_PER_WRITE};

pub(crate) fn cleanup_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = CredisContract::new(ctx.storage.clone());
    deadline_queue::run_through_day(
        &CleanupQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut CredisCleanup::new(ctx),
    )
}

/// The pledge burn of one Credis is one Gratis collateral op.
pub const PLEDGE_BURN_COST: u32 =
    outbe_gratis::enclave_client::COLLATERAL_OP_WITNESS_BYTES.div_ceil(WITNESS_BYTES_PER_WRITE);

struct CredisCleanup<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
}

impl<'a, 's> CredisCleanup<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            ctx,
            credis: CredisContract::new(ctx.storage.clone()),
        }
    }
}

/// A forfeited group's members are its unsettled Credis.
impl DeadlineHandler<B256> for CredisCleanup<'_, '_> {
    type Member = U256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.credis.group_member_count(group)? {
            0 => Due::Drop,
            _ => Due::Handle,
        })
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        self.credis.group_member_count(group)
    }

    fn member_at(&self, group: B256, index: u32) -> Result<U256> {
        self.credis.group_member(group, index)
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    fn member_cost(&self, _group: B256, _credis_id: U256) -> u32 {
        PLEDGE_BURN_COST
    }

    fn handle_member(&mut self, _group: B256, credis_id: U256) -> Result<()> {
        crate::runtime::delete_forfeited_credis(self.ctx.storage.clone(), credis_id)?;
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %group, retry_at, "cleanup sweep: group deferred");
        Ok(())
    }
}
