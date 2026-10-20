//! Native Lysis capacity allocation and auction dispatch.

use alloy_primitives::U256;
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};
use outbe_promislimit::PromisLimitContract;

use crate::errors::MetadosisError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LimitSplit {
    /// The day's own emission plus what it drew from the accumulator.
    pub day_limit: U256,
    /// Maximum Lysis capacity, in protocol units (1e6 per whole COEN).
    pub lysis_limit_minor: U256,
    /// Maximum Desis capacity, not actual Intex issuance, in protocol units.
    pub desis_limit_minor: U256,
    /// What Lysis left of the day's own emission, credited before the auction draws.
    pub carry_over_credit: U256,
}

impl LimitSplit {
    /// The day's own emission bounds Lysis, and the accumulator receives the credit for what
    /// Lysis leaves. The auction then draws from that accumulator: no more than the nominal beyond
    /// the symbolic share, and no more than the accumulator holds.
    pub(crate) fn derive(
        base_limit: U256,
        lysis_limit_minor: U256,
        nominal_total: U256,
        carry_over_before: U256,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidLysisLimitSplit {
            day_limit: base_limit,
            lysis_limit_minor,
        };
        let carry_over_credit = base_limit
            .checked_sub(lysis_limit_minor)
            .ok_or_else(invalid)?;
        let desis_limit_minor = crate::settlement::desis_limit(
            nominal_total,
            lysis_limit_minor,
            base_limit,
            carry_over_before,
        )
        .ok_or_else(invalid)?;
        Self::assemble(
            base_limit,
            lysis_limit_minor,
            desis_limit_minor,
            carry_over_credit,
        )
    }

    fn assemble(
        base_limit: U256,
        lysis_limit_minor: U256,
        desis_limit_minor: U256,
        carry_over_credit: U256,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidLysisLimitSplit {
            day_limit: base_limit,
            lysis_limit_minor,
        };
        if lysis_limit_minor.checked_add(carry_over_credit) != Some(base_limit) {
            return Err(invalid().into());
        }
        let day_limit = base_limit
            .checked_add(desis_limit_minor)
            .ok_or_else(invalid)?;
        Ok(Self {
            day_limit,
            lysis_limit_minor,
            desis_limit_minor,
            carry_over_credit,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LysisLimitInput {
    pub base_limit: U256,
    pub lysis_limit_minor: U256,
    pub nominal_total: U256,
}

/// Reserve the successful day's allocation within the owning command checkpoint.
pub(crate) fn apply_lysis_limit(
    storage: StorageHandle<'_>,
    input: LysisLimitInput,
) -> Result<LimitSplit> {
    let carry_before = PromisLimitContract::new(storage.clone()).get_total_unallocated()?;
    let split = LimitSplit::derive(
        input.base_limit,
        input.lysis_limit_minor,
        input.nominal_total,
        carry_before,
    )?;
    if !split.carry_over_credit.is_zero() {
        let delta = PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(split.carry_over_credit)?;
        if delta.credited != split.carry_over_credit {
            return Err(MetadosisError::LysisLimitCreditMismatch.into());
        }
    }
    if !split.desis_limit_minor.is_zero() {
        let mut promis = PromisLimitContract::new(storage);
        if promis
            .checked_take_carry_over(split.desis_limit_minor)?
            .is_none()
        {
            return Err(MetadosisError::DesisLimitUnavailable {
                desis_limit_minor: split.desis_limit_minor,
                available: promis.get_total_unallocated()?,
            }
            .into());
        }
    }
    Ok(split)
}

pub(crate) fn apply_auction_brief(
    storage: StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    split: &LimitSplit,
    logical_anchor: u64,
) -> Result<()> {
    let is_active = !split.desis_limit_minor.is_zero();
    storage.with_checkpoint(|| {
        outbe_desis::api::dispatch_auction_brief(
            storage.clone(),
            outbe_desis::api::AuctionBrief {
                worldwide_day,
                desis_limit_minor: split.desis_limit_minor,
                is_green: is_active,
            },
            logical_anchor,
            outbe_desis::api::BriefOverflowPolicy::Reject,
        )
        .map(|_| ())
    })
}
