//! Forfeits the called Credis whose settlement window lapsed, earliest deadline first.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_credis::{CredisContract, CredisState, ExpiryHours};
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use crate::precompile::ICredisFactory::ExpiryDeferred;
use crate::runtime;

/// Runs from CycleTick every block. Returns the number of Credis forfeited.
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = CredisContract::new(ctx.storage.clone());
    let mut expiry = CredisExpiry {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        forfeited: 0,
    };
    let mut budget = SweepBudget::per_block();
    expiry_queue::sweep(
        &ExpiryHours(&queue),
        &ctx.storage,
        ctx.block.timestamp,
        &mut budget,
        &mut expiry,
    )?;
    Ok(expiry.forfeited)
}

struct CredisExpiry<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
    forfeited: u32,
}

/// A Credis is its own and only member.
impl ExpiryHandler<U256> for CredisExpiry<'_, '_> {
    type Member = U256;

    fn due(&mut self, credis_id: U256) -> Result<Due> {
        let Some(record) = self.credis.records.get(credis_id)? else {
            return Ok(Due::Drop);
        };
        let forfeitable = record.lifecycle_state()? == CredisState::Called
            && !record.outstanding_principal_minor.is_zero();
        Ok(if forfeitable { Due::Expire } else { Due::Drop })
    }

    fn member_count(&self, _credis_id: U256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, credis_id: U256, _index: u32) -> Result<U256> {
        Ok(credis_id)
    }

    fn expire_member(&mut self, credis_id: U256, _member: U256) -> Result<()> {
        runtime::forfeit_credis(self.ctx.storage.clone(), credis_id)?;
        self.forfeited = self.forfeited.saturating_add(1);
        Ok(())
    }

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        forfeit_failure(error)
    }

    fn deferred(&mut self, credis_id: U256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %credis_id, retry_at, "expiry sweep: forfeit deferred");
        self.ctx.storage.emit_event(
            CREDIS_FACTORY_ADDRESS,
            SolEvent::encode_log_data(&ExpiryDeferred {
                credisId: credis_id,
                retryAt: retry_at,
            }),
        )
    }
}

/// The Gratis enclave client reports its own outage and a deterministic Gratis failure
/// alike as `Fatal`. Until they part, a `Fatal` forfeit fails the block: skipping an
/// outage would fork the chain silently.
pub(crate) fn forfeit_failure(error: &PrecompileError) -> SweepFailure {
    match error {
        PrecompileError::Fatal(_) => SweepFailure::Propagate,
        other => other.sweep_failure(),
    }
}
