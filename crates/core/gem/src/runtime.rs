use alloy_primitives::{B256, U256};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::call_breach::BreachTerms;
use outbe_primitives::error::Result;

use crate::errors::GemError;
use crate::precompile::IGem::GemExpired;
use crate::schema::{GemContract, GemState};

impl GemContract<'_> {
    /// Call an uncalled bucket whose reference-currency daily VWAP exceeded its call
    /// price on at least its threshold days of its trailing window. The VWAPs are read
    /// from `window` (newest-first `(day, vwap)` pairs from the bucket's own `COEN/<iso>`
    /// pair). Returns true if called.
    ///
    /// The terms were sealed when the bucket opened, so a later change to
    /// `CALL_WINDOW`/`CALL_THRESHOLD` cannot re-term it.
    pub(crate) fn trigger_bucket_call(
        &mut self,
        window: &CallWindow,
        bucket: B256,
        now_ts: u64,
    ) -> Result<bool> {
        if self.bucket_gem_count.read(&bucket)? == 0 || self.bucket_called_at.read(&bucket)? != 0 {
            return Ok(false);
        }
        let terms = self.read_bucket_terms(bucket)?;
        let breached = window.breached(&BreachTerms {
            call_price: terms.call_price_minor,
            window_seconds: terms.call_window_seconds,
            threshold_seconds: terms.call_threshold_seconds,
            start_day: terms.start_day,
        });
        if !breached {
            return Ok(false);
        }
        self.mark_bucket_called(bucket, &terms, now_ts)?;
        Ok(true)
    }

    /// Forfeit-burn a Called gem whose Call Notice Period has lapsed. No-op
    /// unless the gem is Called and past `called_at + call_notice_period_seconds`.
    /// Returns true if burned.
    pub(crate) fn forfeit(&mut self, gem_id: U256, now_ts: u64) -> Result<bool> {
        let item = self.get_gem(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Called as u8 {
            return Ok(false);
        }
        let deadline = item.called_at + u64::from(item.call_notice_period_seconds);
        if now_ts <= deadline {
            return Ok(false);
        }
        self.burn(&item)?;
        // The load came from a daily emission sink and nobody realized it, so it is
        // returned. The return uses the same checkpoint as the burn, or there is nothing
        // to recover.
        outbe_promislimit::PromisLimitContract::new(self.storage.clone())
            .add_to_total_unallocated(item.promis_load_minor)?;
        self.emit(GemExpired {
            gemId: gem_id,
            owner: item.owner,
            promisLoadMinor: item.promis_load_minor,
        })?;
        Ok(true)
    }
}
