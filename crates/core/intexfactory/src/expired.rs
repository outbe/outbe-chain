//! Expiry sweep: closes a called group's settlement window, returns the Promis load of
//! everything left unrealized to the unallocated limit, and hands the group to the
//! cleanup.

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
    storage::StorageHandle,
};

use outbe_intex::precompile::IIntex;
use outbe_intex::{ExpiryHours, IntexContract};

use crate::errors::IntexFactoryError;
use crate::runtime::emit_event;

/// Expire every group whose settlement window has closed, earliest bucket first.
/// Returns the groups expired.
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = IntexContract::new(ctx.storage.clone());
    let mut expiry = IntexExpiry {
        storage: &ctx.storage,
        intex: IntexContract::new(ctx.storage.clone()),
        expired: 0,
    };
    expiry_queue::sweep_block(&ExpiryHours(&queue), ctx, &mut expiry)?;
    Ok(expiry.expired)
}

struct IntexExpiry<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    intex: IntexContract<'storage>,
    expired: u32,
}

/// A called group expires as one: it is its own and only member.
impl ExpiryHandler<B256> for IntexExpiry<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.intex.group_member_count(group)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, _group: B256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, group: B256, _index: u32) -> Result<B256> {
        Ok(group)
    }

    /// Returns the group's unrealized load to the pool in one credit.
    fn expire_member(&mut self, group: B256, _member: B256) -> Result<()> {
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
        self.intex.emit(IIntex::IntexGroupExpired {
            groupKey: group,
            forfeitedUnits: forfeited_units,
            returnedPromisMinor: returned,
        })?;
        self.expired = self.expired.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::intexfactory", %group, retry_at, "expiry sweep: group deferred");
        emit_event(
            self.storage,
            crate::precompile::IIntexFactory::ExpiryDeferred {
                groupKey: group,
                retryAt: retry_at,
            },
        )
    }
}
