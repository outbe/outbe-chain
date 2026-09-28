use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::first_full_day;

use crate::errors::GemError;
use crate::precompile::IGem::{GemCalled, GemExpired};
use crate::schema::{GemContract, GemState};

impl GemContract<'_> {
    /// `Issued -> Called` when the coen daily VWAP exceeded this gem's Call
    /// Threshold on at least `call_threshold_seconds` of its trailing `call_window_seconds`,
    /// read off `window` (newest-first `(day, vwap)` pairs). No-op unless the
    /// gem is Issued. Returns true if called.
    ///
    /// Both terms are per-gem snapshots taken at issuance, so a later change to
    /// `CALL_WINDOW`/`CALL_THRESHOLD` cannot re-term a live gem. `window` must
    /// come from this gem's own `reference_currency` pair - the caller selects
    /// it (`hooks::window_for`).
    pub(crate) fn trigger_call(
        &mut self,
        window: &[(u32, Option<U256>)],
        gem_id: U256,
        now_ts: u64,
    ) -> Result<bool> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Issued as u8 {
            return Ok(false);
        }
        // Both terms are stored in seconds; the daily scan needs day counts.
        let window_days = item.call_window_seconds / 86_400;
        let threshold_days = item.call_threshold_seconds / 86_400;
        let issued_day = first_full_day(item.issued_at);
        let mut breaches: u32 = 0;
        for (day, vwap) in window.iter().take(window_days as usize) {
            if *day < issued_day {
                break;
            }
            if let Some(v) = vwap {
                if *v > item.call_price_minor {
                    breaches += 1;
                }
            }
        }
        if breaches < threshold_days {
            return Ok(false);
        }

        self.mark_called(gem_id, now_ts)?;
        self.emit(GemCalled {
            gemId: gem_id,
            calledAt: now_ts,
        })?;
        Ok(true)
    }

    /// Forfeit-burn a Called gem whose Call Notice Period has lapsed. No-op
    /// unless the gem is Called and past `called_at + call_notice_period_seconds`.
    /// Returns true if burned.
    pub(crate) fn forfeit(&mut self, gem_id: U256, now_ts: u64) -> Result<bool> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
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
            promisLoad: item.promis_load_minor,
        })?;
        Ok(true)
    }
}
