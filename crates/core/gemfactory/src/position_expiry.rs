//! Position expiry: a day of positions returns the capacity they never issued in one
//! credit, and the deletion then deletes them.

use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_system_common::pass_budget::PassBudget;

use crate::errors::GemFactoryError;
use crate::precompile::IGemFactory::GemFactoryPositionExpired;
use crate::runtime::emit_event;
use crate::schema::GemFactoryContract;
use crate::state::{
    position_queue_expiry_day, GemFactoryPositionDeletionQueue, GemFactoryPositionExpiryQueue,
};
use alloy_primitives::U256;

pub(crate) fn expire_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = GemFactoryContract::new(ctx.storage.clone());
    deadline_queue::run_through_day(
        &GemFactoryPositionExpiryQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut GemFactoryPositionExpiry::new(ctx),
    )
}

/// Deletes the positions of the expired days. Returns the positions deleted.
pub fn delete_expired_positions(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemFactoryContract::new(ctx.storage.clone());
    let mut deletion = GemFactoryPositionDeletion {
        storage: &ctx.storage,
        factory: GemFactoryContract::new(ctx.storage.clone()),
        deleted: 0,
    };
    deadline_queue::run_with_saved_budget(
        &GemFactoryPositionDeletionQueue(&queue),
        ctx,
        &queue.deletion_budget,
        &mut deletion,
    )?;
    Ok(deletion.deleted)
}

struct GemFactoryPositionExpiry<'a, 's> {
    storage: &'a StorageHandle<'s>,
    factory: GemFactoryContract<'s>,
}

impl<'a, 's> GemFactoryPositionExpiry<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            factory: GemFactoryContract::new(ctx.storage.clone()),
        }
    }
}

/// A day expires as one: it is its own and only member.
impl DeadlineHandler<u64> for GemFactoryPositionExpiry<'_, '_> {
    type Member = u64;

    fn due(&mut self, entry: u64) -> Result<Due> {
        Ok(
            match self
                .factory
                .expiry_day_position_count(position_queue_expiry_day(entry)?)?
            {
                0 => Due::Drop,
                _ => Due::Handle,
            },
        )
    }

    fn member_count(&self, _entry: u64) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, entry: u64, _index: u32) -> Result<u64> {
        Ok(entry)
    }

    /// Nobody issued the capacity, so it goes back to the Promis reserve.
    fn handle_member(&mut self, entry: u64, _member: u64) -> Result<()> {
        let day = position_queue_expiry_day(entry)?;
        let returned = self.factory.expiry_day_capacity_minor.read(&day)?;
        if !returned.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .add(outbe_promislimit::LimitSource::GemFactoryPosition, returned)?;
        }
        self.factory.expiry_day_capacity_minor.clear(&day)?;
        self.factory.hand_expiry_day_to_deletion(day)
    }

    fn deferred(&mut self, entry: u64, retry_at: u64) -> Result<()> {
        let day = position_queue_expiry_day(entry)?;
        tracing::warn!(target: "outbe::gemfactory", day, retry_at, "position expiry: day deferred");
        Ok(())
    }
}

struct GemFactoryPositionDeletion<'a, 's> {
    storage: &'a StorageHandle<'s>,
    factory: GemFactoryContract<'s>,
    deleted: u32,
}

/// An expired day's members are its positions.
impl DeadlineHandler<u64> for GemFactoryPositionDeletion<'_, '_> {
    type Member = U256;

    fn due(&mut self, entry: u64) -> Result<Due> {
        Ok(
            match self
                .factory
                .expiry_day_position_count(position_queue_expiry_day(entry)?)?
            {
                0 => Due::Drop,
                _ => Due::Handle,
            },
        )
    }

    fn member_count(&self, entry: u64) -> Result<u32> {
        self.factory
            .expiry_day_position_count(position_queue_expiry_day(entry)?)
    }

    fn member_at(&self, entry: u64, index: u32) -> Result<U256> {
        self.factory
            .expiry_day_position(position_queue_expiry_day(entry)?, index)
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    fn handle_member(&mut self, _entry: u64, gem_factory_position_id: U256) -> Result<()> {
        let record = self
            .factory
            .positions
            .get(gem_factory_position_id)?
            .ok_or(GemFactoryError::GemFactoryPositionNotFound)?;
        self.factory.delete_position(&record)?;
        emit_event(
            self.storage,
            GemFactoryPositionExpired {
                gemFactoryPositionId: gem_factory_position_id,
                owner: record.owner,
            },
        )?;
        self.deleted = self.deleted.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, entry: u64, retry_at: u64) -> Result<()> {
        let day = position_queue_expiry_day(entry)?;
        tracing::warn!(target: "outbe::gemfactory", day, retry_at, "position deletion: day deferred");
        Ok(())
    }
}
