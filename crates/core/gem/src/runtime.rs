use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::error::{PrecompileError, Result};

use crate::constants::{GEM_CALL_PERIOD_SECONDS, GEM_CALL_THRESHOLD_DAYS, QUALIFIER_REFERENCE_ISO};
use crate::errors::GemError;
use crate::events::{GemBurned, GemCalled, GemQualified};
use crate::schema::{GemContract, GemState};

impl GemContract<'_> {
    pub(crate) fn qualify(&mut self, gem_id: U256, now: u64, rate: U256) -> Result<bool> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Issued as u8 {
            return Ok(false);
        }
        // `rate` is COEN/<QUALIFIER_REFERENCE_ISO>; floor_price is denominated
        // in the gem's own reference_currency. Skip silently if they don't
        // match so we don't promote against an unrelated rate.
        if item.reference_currency != QUALIFIER_REFERENCE_ISO {
            return Ok(false);
        }
        if rate <= item.floor_price {
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
    /// Threshold on at least `GEM_CALL_THRESHOLD_DAYS` of the trailing `window`
    /// (newest-first `(day, vwap)` pairs). No-op unless the gem is Qualified
    /// against the qualifier pair. Returns true if called.
    pub(crate) fn call(
        &mut self,
        window: &[(WorldwideDay, Option<U256>)],
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
        let issued_wwd = WorldwideDay::from_timestamp(item.issued_at);
        let mut breaches: u32 = 0;
        for (day, vwap) in window {
            if *day < issued_wwd {
                break;
            }
            if let Some(v) = vwap {
                if *v > item.call_threshold {
                    breaches += 1;
                }
            }
        }
        if breaches < u32::from(GEM_CALL_THRESHOLD_DAYS) {
            return Ok(false);
        }

        // u32 timestamp; bounded until 2106 (matches issued_at semantics).
        let called_at = u32::try_from(now_ts)
            .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;
        self.mark_called(gem_id, called_at)?;
        self.emit(GemCalled {
            gemId: gem_id,
            calledAt: called_at,
        })?;
        Ok(true)
    }

    /// Forfeit-burn a Called gem whose Call Notice Period has lapsed. No-op
    /// unless the gem is Called and past `called_at + GEM_CALL_PERIOD_SECONDS`.
    /// Returns true if burned.
    pub(crate) fn forfeit(&mut self, gem_id: U256, now_ts: u64) -> Result<bool> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Called as u8 {
            return Ok(false);
        }
        let deadline = u64::from(item.called_at) + u64::from(GEM_CALL_PERIOD_SECONDS);
        if now_ts <= deadline {
            return Ok(false);
        }
        self.burn(&item)?;
        self.emit(GemBurned {
            gemId: gem_id,
            owner: item.owner,
            gemLoad: item.gem_load,
        })?;
        Ok(true)
    }
}
