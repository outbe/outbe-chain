use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{Result, SweepFailure},
    expiry_queue::{self, ExpiryHandler, Step},
    storage::StorageHandle,
    time::WorldwideDay,
};

use super::{materializing, sweep_failure};
use crate::{
    api, constants::MAX_NOD_FORFEITS_PER_BLOCK, precompile::INod, schema::NodContract,
    state::ExpiryHours,
};

/// The storage, scope and parent bodies a forfeit loads and removes Nods through.
pub(crate) struct Bodies<'a, 's, P> {
    pub(crate) storage: &'a StorageHandle<'s>,
    pub(crate) scope: &'a ExecutionScope,
    pub(crate) parent: &'a P,
}

/// Forfeit-burns the unpaid Nods of the called buckets whose notice period lapsed.
/// Returns the Nods burned.
pub(crate) fn sweep_expired(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let mut expiry = NodExpiry {
        bodies: Bodies {
            storage: &ctx.storage,
            scope,
            parent,
        },
        nod: NodContract::new(ctx.storage.clone()),
        now: ctx.block.timestamp,
        forfeited: 0,
    };
    expiry.run(MAX_NOD_FORFEITS_PER_BLOCK)?;
    Ok(expiry.forfeited)
}

/// One block's burns across the due hours.
struct NodExpiry<'a, 's, P> {
    bodies: Bodies<'a, 's, P>,
    nod: NodContract<'s>,
    now: u64,
    forfeited: u32,
}

impl<P: ParentBodySource> ExpiryHandler<B256> for NodExpiry<'_, '_, P> {
    /// Burns a lapsed bucket's unpaid Nods, one budget unit each. A bucket whose
    /// Nods are still landing waits for the next hour.
    fn expire(&mut self, bucket_key: B256, budget: &mut u32) -> Result<Step> {
        if materializing(&self.nod, bucket_key)? {
            *budget -= 1;
            self.defer(bucket_key)?;
            return Ok(Step::Done);
        }
        // Paid entitlements retain their bucket terms, but cannot be forfeited.
        if self.nod.bucket_nod_count.read(&bucket_key)? == 0 {
            *budget -= 1;
            self.nod.remove_called_bucket(bucket_key)?;
            return Ok(Step::Done);
        }
        let limit = *budget;
        let bodies = &self.bodies;
        let nod = &mut self.nod;
        let burned = bodies
            .storage
            .with_checkpoint(|| forfeit_members(bodies, nod, bucket_key, limit));
        match burned {
            Ok(burned) => {
                *budget -= burned;
                self.forfeited = self.forfeited.saturating_add(burned);
                // Members left: the budget or the gas ran out, so the next block resumes here.
                if self.nod.bucket_nod_count.read(&bucket_key)? != 0 {
                    return Ok(Step::Hold);
                }
                self.nod.remove_called_bucket(bucket_key)?;
                Ok(Step::Done)
            }
            Err(error) => match sweep_failure(&error) {
                SweepFailure::Skip => {
                    *budget -= 1;
                    tracing::warn!(target: "outbe::nod", %bucket_key, error = ?error, "forfeit sweep: bucket not forfeited, retrying next hour");
                    self.defer(bucket_key)?;
                    Ok(Step::Done)
                }
                SweepFailure::Stop => Ok(Step::Hold),
                SweepFailure::Propagate => Err(error),
            },
        }
    }

    fn retire_leftover(&mut self, bucket_key: B256) -> Result<()> {
        tracing::warn!(target: "outbe::nod", %bucket_key, "forfeit sweep: hour outlived itself, retrying next hour");
        self.defer(bucket_key)
    }
}

impl<P: ParentBodySource> NodExpiry<'_, '_, P> {
    fn run(&mut self, mut budget: u32) -> Result<()> {
        let queue = NodContract::new(self.bodies.storage.clone());
        expiry_queue::sweep(&ExpiryHours(&queue), self.now, &mut budget, self)
    }

    fn defer(&self, bucket_key: B256) -> Result<()> {
        let next = expiry_queue::bucket_of(self.now).saturating_add(1);
        expiry_queue::retarget(&ExpiryHours(&self.nod), bucket_key, next)
    }
}

/// Forfeit-burns up to `budget` of a lapsed bucket's remaining unpaid Nods, newest
/// first. Returns how many were burned.
///
/// A bucket holding more members than the budget resumes on the next block. The
/// resume cannot change an outcome. The deadline has already passed and settlement is
/// closed, so nothing can rescue the remainder.
/// The bucket body remains while settled members exist. Removal requires both
/// unpaid and settled member counts to reach zero.
///
/// Each member burns in its own checkpoint, so running out of gas stops the batch
/// early and keeps the members already burned.
///
/// Each burned load returns to the Promis Reserve. Lysis drew it out of the day
/// limit, and only mining converts it into Gratis. Without this return, a load
/// destroyed unmined would leave the reserve with nothing minted against it.
/// The credit is one accumulated write per pass, and the caller's checkpoint
/// makes it atomic with the burns it accounts for.
pub(crate) fn forfeit_members(
    bodies: &Bodies<'_, '_, impl ParentBodySource>,
    nod: &mut NodContract<'_>,
    bucket_key: B256,
    budget: u32,
) -> Result<u32> {
    let storage = bodies.storage;
    let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
    let mut burned: u32 = 0;
    let mut credit = U256::ZERO;
    while burned < budget {
        let member =
            storage.with_checkpoint(|| forfeit_last_member(bodies, nod, bucket_key, worldwide_day));
        let gratis_load_minor = match member {
            Ok(Some(load)) => load,
            Ok(None) => break,
            Err(error) if sweep_failure(&error) == SweepFailure::Stop => break,
            Err(error) => return Err(error),
        };
        credit = credit.checked_add(gratis_load_minor).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Revert(
                "Nod forfeit Promis Reserve credit overflow".into(),
            )
        })?;
        burned = burned.saturating_add(1);
    }
    if !credit.is_zero() {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .add_to_total_unallocated(credit)?;
    }
    Ok(burned)
}

/// Burns the bucket's newest unpaid member and returns its load, or `None` when none is left.
fn forfeit_last_member(
    bodies: &Bodies<'_, '_, impl ParentBodySource>,
    nod: &mut NodContract<'_>,
    bucket_key: B256,
    worldwide_day: WorldwideDay,
) -> Result<Option<U256>> {
    let Bodies {
        storage,
        scope,
        parent,
    } = *bodies;
    let count = nod.bucket_nod_count.read(&bucket_key)?;
    let Some(last) = count.checked_sub(1) else {
        return Ok(None);
    };
    let nod_id = nod
        .bucket_nods
        .read(&NodContract::bucket_nod_key(bucket_key, last))?;
    if nod_id.is_zero() {
        return Err(outbe_primitives::error::PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} member slot {last} is empty during forfeit"
        )));
    }
    let item = api::load_item(storage, scope, parent, nod_id)?.ok_or_else(|| {
        outbe_primitives::error::PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} member {nod_id} has no body during forfeit"
        ))
    })?;
    if item.body().is_settled || item.body().bucket_key != bucket_key {
        return Err(outbe_primitives::error::PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} indexes an ineligible member {nod_id}"
        )));
    }
    let owner = item.body().owner;
    let gratis_load_minor = item.body().gratis_load_minor;
    let bucket_id = WwdEntityId::from_day_and_digest(worldwide_day, bucket_key.0);
    let bucket = api::load_bucket(storage, scope, parent, bucket_id)?.ok_or_else(|| {
        outbe_primitives::error::PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} has no body during forfeit"
        ))
    })?;
    api::remove_nod(storage, scope, item, bucket)?;
    nod.emit(INod::NodForfeited {
        owner,
        nodId: nod_id.to_u256(),
        gratisLoadMinor: gratis_load_minor,
    })?;
    Ok(Some(gratis_load_minor))
}
