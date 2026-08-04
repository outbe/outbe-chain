use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::timestamp_to_date_key;

use crate::constants::{CALL_THRESHOLD_DAYS, QUALIFIER_REFERENCE_ISO};
use crate::errors::GemError;
use crate::events::{GemBurned, GemCalled, GemQualified};
use crate::schema::{GemContract, GemState};

impl GemContract<'_> {
    pub(crate) fn qualify(&mut self, gem_id: U256, now: u64, rate: U256) -> Result<bool> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Issued as u8 {
            return Ok(false);
        }
        // `rate` is COEN/<QUALIFIER_REFERENCE_ISO>; floor_price_minor is denominated
        // in the gem's own reference_currency. Skip silently if they don't
        // match so we don't promote against an unrelated rate.
        if item.reference_currency != QUALIFIER_REFERENCE_ISO {
            return Ok(false);
        }
        if rate <= item.floor_price_minor {
            return Ok(false);
        }
        self.set_state(gem_id, GemState::Qualified)?;
        self.emit(GemQualified {
            gemId: gem_id,
            qualifiedAt: now,
        })?;
        Ok(true)
    }

    /// `Qualified -> Called` when the coen daily VWAP exceeded this gem's Call
    /// Threshold on at least `CALL_THRESHOLD_DAYS` of the trailing `window`
    /// (newest-first `(day, vwap)` pairs). No-op unless the gem is Qualified
    /// against the qualifier pair. Returns true if called.
    pub(crate) fn trigger_call(
        &mut self,
        window: &[(u32, Option<U256>)],
        gem_id: U256,
        now_ts: u64,
    ) -> Result<bool> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Qualified as u8 {
            return Ok(false);
        }
        if item.reference_currency != QUALIFIER_REFERENCE_ISO {
            return Ok(false);
        }
        let issued_day = timestamp_to_date_key(item.issued_at);
        let mut breaches: u32 = 0;
        for (day, vwap) in window {
            if *day < issued_day {
                break;
            }
            if let Some(v) = vwap {
                if *v > item.call_price_minor {
                    breaches += 1;
                }
            }
        }
        if breaches < u32::from(CALL_THRESHOLD_DAYS) {
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
    /// unless the gem is Called and past `called_at + call_notice_period`.
    /// Returns true if burned.
    pub(crate) fn forfeit(&mut self, gem_id: U256, now_ts: u64) -> Result<bool> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Called as u8 {
            return Ok(false);
        }
        let deadline = item.called_at + u64::from(item.call_notice_period) * 86_400;
        if now_ts <= deadline {
            return Ok(false);
        }
        self.burn(&item)?;
        self.emit(GemBurned {
            gemId: gem_id,
            owner: item.owner,
            gemLoad: item.gem_load_minor,
        })?;
        Ok(true)
    }
}
