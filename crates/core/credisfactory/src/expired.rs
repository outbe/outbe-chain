//! Voids the called positions whose settlement window lapsed, earliest deadline first.

use alloy_primitives::U256;
use outbe_credis::{CredisContract, CredisState, ExpiryHours};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{Result, SweepFailure},
    expiry_queue::{self, ExpiryHandler, Step},
};

use crate::runtime;

/// Max queue steps per block. Each void makes a blocking TEE round-trip.
pub(crate) const MAX_CREDIS_VOIDS_PER_BLOCK: u32 = 64;

/// Runs from CycleTick every block. Returns the number of positions voided.
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut expiry = CredisExpiry {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        voided: 0,
    };
    expiry.run(MAX_CREDIS_VOIDS_PER_BLOCK)?;
    Ok(expiry.voided)
}

struct CredisExpiry<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
    voided: u32,
}

impl ExpiryHandler<U256> for CredisExpiry<'_, '_> {
    fn expire(&mut self, position_id: U256, budget: &mut u32) -> Result<Step> {
        *budget -= 1;
        if !self.voidable(position_id)? {
            expiry_queue::remove(&ExpiryHours(&self.credis), position_id)?;
            return Ok(Step::Done);
        }
        // A failed void is not skipped: its TEE round-trip fails on node-local
        // faults, and swallowing one would fork the chain silently.
        match runtime::void_position(self.ctx.storage.clone(), position_id) {
            Ok(()) => {
                self.voided = self.voided.saturating_add(1);
                Ok(Step::Done)
            }
            Err(error) if error.sweep_failure() == SweepFailure::Stop => Ok(Step::Hold),
            Err(error) => Err(error),
        }
    }

    fn retire_leftover(&mut self, position_id: U256) -> Result<()> {
        let queue = ExpiryHours(&self.credis);
        if !self.voidable(position_id)? {
            return expiry_queue::remove(&queue, position_id);
        }
        tracing::warn!(target: "outbe::credisfactory", %position_id, "expiry sweep: hour outlived itself, retrying next hour");
        let next = expiry_queue::bucket_of(self.ctx.block.timestamp).saturating_add(1);
        expiry_queue::retarget(&queue, position_id, next)
    }
}

impl CredisExpiry<'_, '_> {
    fn run(&mut self, mut budget: u32) -> Result<()> {
        let queue = CredisContract::new(self.ctx.storage.clone());
        let now = self.ctx.block.timestamp;
        expiry_queue::sweep(&ExpiryHours(&queue), now, &mut budget, self)
    }

    fn voidable(&self, position_id: U256) -> Result<bool> {
        let position = self.credis.get_position(position_id)?;
        Ok(position.lifecycle_state()? == CredisState::Called
            && !position.outstanding_principal_minor.is_zero())
    }
}
