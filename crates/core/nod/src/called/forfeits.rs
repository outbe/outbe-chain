use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{Result, SweepFailure},
    storage::StorageHandle,
    time::WorldwideDay,
};

use super::{materializing, sweep_failure};
use crate::{
    api,
    constants::{MAX_NOD_CALL_VISITS_PER_BLOCK, MAX_NOD_FORFEITS_PER_BLOCK},
    precompile::INod,
    schema::NodContract,
};

/// The storage, scope and parent bodies a forfeit loads and removes Nods through.
pub(crate) struct Bodies<'a, 's, P> {
    pub(crate) storage: &'a StorageHandle<'s>,
    pub(crate) scope: &'a ExecutionScope,
    pub(crate) parent: &'a P,
}

/// Returns the Nods burned and whether the walk reached the bottom.
pub(super) fn forfeit_arm(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod: &mut NodContract<'_>,
    visits: &mut u32,
) -> Result<(u32, bool)> {
    let len = nod.called_buckets.len()?;
    if len == 0 {
        return Ok((0, true));
    }
    // Stored as `index + 1`; 0 means "start a fresh pass from the top".
    let initial_cursor = nod.forfeit_cursor.read()?;
    let mut cursor = start_index(initial_cursor, len);
    let mut pass = ForfeitPass {
        bodies: Bodies {
            storage: &ctx.storage,
            scope,
            parent,
        },
        now: ctx.block.timestamp,
        forfeited: 0,
    };

    // Descending walk: removing a bucket swap-pops the tail into the hole. The
    // tail is already behind a descending cursor, so the walk skips no live
    // entry and visits none twice.
    let completed = loop {
        if *visits >= MAX_NOD_CALL_VISITS_PER_BLOCK {
            break false;
        }
        if let Some(bucket_key) = nod.called_buckets.get(cursor)? {
            *visits += 1;
            if !pass.visit(nod, bucket_key)? {
                break false;
            }
        }
        if cursor == 0 {
            break true;
        }
        cursor -= 1;
    };

    let next_cursor = stored_cursor(completed, cursor);
    if next_cursor != initial_cursor {
        nod.forfeit_cursor.write(next_cursor)?;
    }
    Ok((pass.forfeited, completed))
}

fn start_index(stored_cursor: u32, len: u32) -> u32 {
    match stored_cursor {
        0 => len - 1,
        resume => resume.saturating_sub(1).min(len - 1),
    }
}

fn stored_cursor(completed: bool, index: u32) -> u32 {
    if completed {
        0
    } else {
        index.saturating_add(1)
    }
}

/// One forfeit walk's body access, clock and running burn count.
struct ForfeitPass<'a, 's, P> {
    bodies: Bodies<'a, 's, P>,
    now: u64,
    forfeited: u32,
}

impl<P: ParentBodySource> ForfeitPass<'_, '_, P> {
    /// Burns a lapsed bucket's unpaid Nods. Returns whether the walk goes on past it.
    fn visit(&mut self, nod: &mut NodContract<'_>, bucket_key: B256) -> Result<bool> {
        let called_at = nod.bucket_called_at.read(&bucket_key)?;
        // Paid entitlements retain their bucket terms, but cannot be forfeited.
        let has_unpaid = nod.bucket_nod_count.read(&bucket_key)? != 0;
        // A bucket whose Nods are still landing stays listed for a later pass.
        let lapsed = has_unpaid
            && self.now > api::settlement_deadline_of(called_at, notice_period(nod, bucket_key)?)
            && !materializing(nod, bucket_key)?;
        if !lapsed {
            return Ok(true);
        }
        let budget = MAX_NOD_FORFEITS_PER_BLOCK.saturating_sub(self.forfeited);
        if budget == 0 {
            return Ok(false);
        }
        let bodies = &self.bodies;
        let res = bodies
            .storage
            .with_checkpoint(|| forfeit_members(bodies, nod, bucket_key, budget));
        match res {
            Ok(burned) => {
                self.forfeited = self.forfeited.saturating_add(burned);
                // Members left: the budget or the gas ran out, so the next slice resumes here.
                Ok(nod.bucket_nod_count.read(&bucket_key)? == 0)
            }
            Err(error) => match sweep_failure(&error) {
                SweepFailure::Skip => Ok(true),
                SweepFailure::Stop => Ok(false),
                SweepFailure::Propagate => Err(error),
            },
        }
    }
}

/// The bucket's sealed notice period. Read on its own in the forfeit arm, which
/// needs no other term.
fn notice_period(nod: &NodContract<'_>, bucket_key: B256) -> Result<u32> {
    nod.callable_bucket_call_notice_period_seconds
        .read(&bucket_key)
}

/// Forfeit-burns up to `budget` of a lapsed bucket's remaining unpaid Nods, newest
/// first. Returns how many were burned.
///
/// A bucket holding more members than the budget resumes on the next run. The
/// resume cannot change an outcome. The deadline has already passed and settlement is
/// closed, so nothing can rescue the remainder.
/// The bucket body and called-list entry remain while settled members exist.
/// Removal requires both unpaid and settled member counts to reach zero.
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
    let gratis_load_minor = api::calculation_amount(item.body())?;
    let encrypted_gratis_amount = item.body().encrypted.encrypted_gratis_amount.clone();
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
        encryptedGratisAmount: encrypted_gratis_amount.into(),
    })?;
    Ok(Some(gratis_load_minor))
}
