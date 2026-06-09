use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::constants::{MATURITY_PERIOD_SECONDS, QUALIFIER_REFERENCE_ISO};
use crate::errors::GemError;
use crate::events::GemQualified;
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
        // Gate: strictly more than MATURITY_PERIOD_SECONDS (21 days) must
        // have elapsed since `issued_at`. Exactly 21 days is not enough.
        let mature_at = item.issued_at.saturating_add(MATURITY_PERIOD_SECONDS);
        if now <= mature_at {
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
}
