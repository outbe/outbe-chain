//! Voids the called positions whose settlement window lapsed, earliest deadline first.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_credis::{CredisContract, CredisState, ExpiryHours};
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use crate::precompile::ICredisFactory::ExpiryDeferred;
use crate::runtime;

/// Runs from CycleTick every block. Returns the number of positions voided.
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = CredisContract::new(ctx.storage.clone());
    let mut expiry = CredisExpiry {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        voided: 0,
    };
    let mut budget = SweepBudget::per_block();
    expiry_queue::sweep(
        &ExpiryHours(&queue),
        &ctx.storage,
        ctx.block.timestamp,
        &mut budget,
        &mut expiry,
    )?;
    Ok(expiry.voided)
}

struct CredisExpiry<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
    voided: u32,
}

/// A position is its own and only member.
impl ExpiryHandler<U256> for CredisExpiry<'_, '_> {
    type Member = U256;

    fn due(&mut self, position_id: U256) -> Result<Due> {
        let Some(position) = self.credis.positions.get(position_id)? else {
            return Ok(Due::Drop);
        };
        let voidable = position.lifecycle_state()? == CredisState::Called
            && !position.outstanding_principal_minor.is_zero();
        Ok(if voidable { Due::Expire } else { Due::Drop })
    }

    fn member_count(&self, _position_id: U256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, position_id: U256, _index: u32) -> Result<U256> {
        Ok(position_id)
    }

    fn expire_member(&mut self, position_id: U256, _member: U256) -> Result<()> {
        runtime::void_position(self.ctx.storage.clone(), position_id)?;
        self.voided = self.voided.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, position_id: U256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %position_id, retry_at, "expiry sweep: void deferred");
        self.ctx.storage.emit_event(
            CREDIS_FACTORY_ADDRESS,
            SolEvent::encode_log_data(&ExpiryDeferred {
                positionId: position_id,
                retryAt: retry_at,
            }),
        )
    }
}
