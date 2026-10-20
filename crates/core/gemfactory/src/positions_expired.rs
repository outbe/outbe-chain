//! Position expiry: an hour of positions returns the capacity they never issued in one
//! credit, and the cleanup then deletes them.

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    storage::StorageHandle,
};

use crate::errors::GemFactoryError;
use crate::runtime::emit_event;
use crate::schema::GemFactoryContract;
use crate::state::{PositionCleanupHours, PositionExpiryHours};
use alloy_primitives::U256;

/// Returns the unissued capacity of the hours whose positions expired. Returns the
/// hours expired.
pub fn sweep_expired_positions(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemFactoryContract::new(ctx.storage.clone());
    let mut expiry = PositionExpiry {
        storage: &ctx.storage,
        factory: GemFactoryContract::new(ctx.storage.clone()),
        expired: 0,
    };
    expiry_queue::sweep_block(&PositionExpiryHours(&queue), ctx, &mut expiry)?;
    Ok(expiry.expired)
}

/// Deletes the positions of the expired hours. Returns the positions deleted.
pub fn sweep_position_cleanup(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemFactoryContract::new(ctx.storage.clone());
    let mut cleanup = PositionCleanup {
        factory: GemFactoryContract::new(ctx.storage.clone()),
        deleted: 0,
    };
    expiry_queue::sweep_block(&PositionCleanupHours(&queue), ctx, &mut cleanup)?;
    Ok(cleanup.deleted)
}

struct PositionExpiry<'a, 's> {
    storage: &'a StorageHandle<'s>,
    factory: GemFactoryContract<'s>,
    expired: u32,
}

/// An hour expires as one: it is its own and only member.
impl ExpiryHandler<u64> for PositionExpiry<'_, '_> {
    type Member = u64;

    fn due(&mut self, hour: u64) -> Result<Due> {
        Ok(match self.factory.hour_position_count(hour)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, _hour: u64) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, hour: u64, _index: u32) -> Result<u64> {
        Ok(hour)
    }

    /// Nobody issued the capacity, so it goes back to the Promis reserve.
    fn expire_member(&mut self, hour: u64, _member: u64) -> Result<()> {
        let positions = self.factory.hour_position_count(hour)?;
        let returned = self.factory.hour_capacity_minor.read(&hour)?;
        if !returned.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .add_to_total_unallocated(returned)?;
        }
        self.factory.hour_capacity_minor.clear(&hour)?;
        self.factory.hand_hour_to_cleanup(hour)?;
        emit_event(
            self.storage,
            crate::precompile::IGemFactory::GemPositionGroupExpired {
                expiryHour: hour,
                positions,
                returnedCapacityMinor: returned,
            },
        )?;
        self.expired = self.expired.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, hour: u64, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gemfactory", hour, retry_at, "position expiry: hour deferred");
        Ok(())
    }
}

struct PositionCleanup<'s> {
    factory: GemFactoryContract<'s>,
    deleted: u32,
}

/// An expired hour's members are its positions.
impl ExpiryHandler<u64> for PositionCleanup<'_> {
    type Member = U256;

    fn due(&mut self, hour: u64) -> Result<Due> {
        Ok(match self.factory.hour_position_count(hour)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, hour: u64) -> Result<u32> {
        self.factory.hour_position_count(hour)
    }

    fn member_at(&self, hour: u64, index: u32) -> Result<U256> {
        self.factory.hour_position(hour, index)
    }

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.record_sweep_failure()
    }

    fn expire_member(&mut self, _hour: u64, position_id: U256) -> Result<()> {
        let record = self
            .factory
            .positions
            .get(position_id)?
            .ok_or(GemFactoryError::PositionNotFound)?;
        self.factory.delete_position(&record)?;
        self.deleted = self.deleted.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, hour: u64, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gemfactory", hour, retry_at, "position cleanup: hour deferred");
        Ok(())
    }
}
