use alloy_primitives::{B256, U256};
use outbe_primitives::error::Result;

use crate::errors::GemError;
use crate::precompile::IGem::GemExpired;
use crate::schema::BucketTerms;
use crate::schema::{GemContract, GemState};

impl GemContract<'_> {
    /// Call an uncalled bucket whose reference-currency daily VWAP exceeded its call
    /// price on at least its threshold days of its trailing window, read off `window`
    /// (newest-first `(day, vwap)` pairs from the bucket's own `COEN/<iso>` pair).
    /// Returns true if called.
    ///
    /// The terms were sealed when the bucket opened, so a later change to
    /// `CALL_WINDOW`/`CALL_THRESHOLD` cannot re-term it.
    pub(crate) fn trigger_bucket_call(
        &mut self,
        window: &[(u32, Option<U256>)],
        bucket: B256,
        now_ts: u64,
    ) -> Result<bool> {
        if self.bucket_gem_count.read(&bucket)? == 0 || self.bucket_called_at.read(&bucket)? != 0 {
            return Ok(false);
        }
        let terms = self.read_bucket_terms(bucket)?;
        if !breached_enough(window, &terms) {
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
        // The load came out of a daily emission sink and nobody realized it, so it
        // goes back. Same checkpoint as the burn, or there is nothing to recover.
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

/// Days before the bucket's start day never count: its gems did not exist yet.
fn breached_enough(window: &[(u32, Option<U256>)], terms: &BucketTerms) -> bool {
    // Both terms are stored in seconds; the daily scan needs day counts.
    let window_days = terms.call_window_seconds / 86_400;
    let threshold_days = terms.call_threshold_seconds / 86_400;
    // Zero days means no terms, not a breach on every day.
    if threshold_days == 0 || threshold_days > window_days {
        return false;
    }
    let mut breaches: u32 = 0;
    for (day, vwap) in window.iter().take(window_days as usize) {
        if *day < terms.start_day {
            break;
        }
        if vwap.is_some_and(|value| value > terms.call_price) {
            breaches += 1;
        }
    }
    breaches >= threshold_days
}
