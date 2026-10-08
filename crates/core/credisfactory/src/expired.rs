//! Voids the called positions whose settlement window lapsed, earliest deadline first.

use alloy_primitives::U256;
use outbe_credis::{CredisContract, CredisState, ExpiryHours};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
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
        match runtime::void_position(self.ctx.storage.clone(), position_id) {
            Ok(()) => {
                self.voided = self.voided.saturating_add(1);
                Ok(Step::Done)
            }
            Err(error) => match void_failure(&error) {
                SweepFailure::Propagate => Err(error),
                SweepFailure::Stop => Ok(Step::Hold),
                SweepFailure::Skip => {
                    tracing::warn!(target: "outbe::credisfactory", %position_id, error = ?error, "expiry sweep: void failed, retrying next hour");
                    self.defer(position_id)?;
                    Ok(Step::Done)
                }
            },
        }
    }

    fn retire_leftover(&mut self, position_id: U256) -> Result<()> {
        if !self.voidable(position_id)? {
            return expiry_queue::remove(&ExpiryHours(&self.credis), position_id);
        }
        tracing::warn!(target: "outbe::credisfactory", %position_id, "expiry sweep: hour outlived itself, retrying next hour");
        self.defer(position_id)
    }
}

/// The Gratis enclave client reports its own outage and a deterministic Gratis failure
/// alike as `Fatal`. Until they part, a `Fatal` void fails the block: skipping an
/// outage would fork the chain silently.
pub(crate) fn void_failure(error: &PrecompileError) -> SweepFailure {
    match error {
        PrecompileError::Fatal(_) => SweepFailure::Propagate,
        other => other.sweep_failure(),
    }
}

impl CredisExpiry<'_, '_> {
    fn run(&mut self, mut budget: u32) -> Result<()> {
        let queue = CredisContract::new(self.ctx.storage.clone());
        let now = self.ctx.block.timestamp;
        expiry_queue::sweep(&ExpiryHours(&queue), now, &mut budget, self)
    }

    fn defer(&self, position_id: U256) -> Result<()> {
        let next = expiry_queue::bucket_of(self.ctx.block.timestamp).saturating_add(1);
        expiry_queue::retarget(&ExpiryHours(&self.credis), position_id, next)
    }

    fn voidable(&self, position_id: U256) -> Result<bool> {
        let position = self.credis.get_position(position_id)?;
        Ok(position.lifecycle_state()? == CredisState::Called
            && !position.outstanding_principal_minor.is_zero())
    }
}
