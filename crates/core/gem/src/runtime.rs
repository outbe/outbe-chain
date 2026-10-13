use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::call_breach::BreachTerms;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::first_full_day;

use crate::api;
use crate::precompile::IGem::GemExpired;
use crate::schema::GemContract;

/// The storage, scope and parent bodies a forfeit loads and removes gems through.
pub(crate) struct Bodies<'a, 's, P> {
    pub(crate) storage: &'a StorageHandle<'s>,
    pub(crate) scope: &'a ExecutionScope,
    pub(crate) parent: &'a P,
}

impl GemContract<'_> {
    /// Call an uncalled bucket whose reference-currency daily VWAP exceeded its call
    /// price on at least its threshold days of its trailing window. The VWAPs are read
    /// from `window` (newest-first `(day, vwap)` pairs from the bucket's own `COEN/<iso>`
    /// pair). Returns true if called.
    ///
    /// The terms were sealed when the bucket opened, so a later change to
    /// `CALL_WINDOW`/`CALL_THRESHOLD` cannot re-term it. Days before the first full day
    /// of the first member never count: the bucket did not exist yet.
    pub(crate) fn trigger_bucket_call(
        &mut self,
        window: &CallWindow,
        bucket: B256,
        now_ts: u64,
    ) -> Result<bool> {
        if self.bucket_gem_count.read(&bucket)? == 0 || self.bucket_called_at.read(&bucket)? != 0 {
            return Ok(false);
        }
        let terms = self.read_call_terms(bucket)?;
        let issued_at = self.bucket_issued_at.read(&bucket)?;
        let breached = window.breached(&BreachTerms {
            call_price: terms.call_price_minor,
            window_seconds: terms.call_window_seconds,
            threshold_seconds: terms.call_threshold_seconds,
            start_day: first_full_day(issued_at),
        });
        if !breached {
            return Ok(false);
        }
        self.mark_bucket_called(bucket, &terms, now_ts)?;
        Ok(true)
    }
}

/// Forfeit-burns one unpaid member of a lapsed bucket and returns its load to the
/// Promis reserve. The load came from a daily emission sink and nobody realized it, so
/// it goes back. The return uses the same checkpoint as the burn, or there is nothing
/// to recover.
pub(crate) fn forfeit_member(
    bodies: &Bodies<'_, '_, impl ParentBodySource>,
    gem: &mut GemContract<'_>,
    bucket_key: B256,
    gem_id: U256,
) -> Result<()> {
    let Bodies {
        storage,
        scope,
        parent,
    } = *bodies;
    if gem_id.is_zero() {
        return Err(PrecompileError::Revert(format!(
            "gem bucket {bucket_key} has an empty member slot during forfeit"
        )));
    }
    let (item, bucket) = api::load_pair(storage, scope, parent, gem_id)?.ok_or_else(|| {
        PrecompileError::Revert(format!(
            "gem bucket {bucket_key} member {gem_id} has no body during forfeit"
        ))
    })?;
    if item.body().is_settled() || item.body().bucket_key != bucket_key {
        return Err(PrecompileError::Revert(format!(
            "gem bucket {bucket_key} indexes an ineligible member {gem_id}"
        )));
    }
    let owner = item.body().owner;
    let promis_load_minor = item.body().promis_load_minor;
    api::remove_gem(storage, scope, item, bucket)?;
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(promis_load_minor)?;
    gem.emit(GemExpired {
        gemId: gem_id,
        owner,
        promisLoadMinor: promis_load_minor,
    })
}
