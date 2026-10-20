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
    pub promis_limit_credit: U256,
}

impl LimitSplit {
    /// The day's own emission bounds Lysis, and the accumulator receives the credit for what
    /// Lysis leaves. The auction then draws from that accumulator: no more than the nominal beyond
    /// the symbolic share, and no more than the accumulator holds.
    pub(crate) fn derive(
        base_limit: U256,
        lysis_limit_minor: U256,
        nominal_total: U256,
        promis_limit_before: U256,
        green: bool,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidLysisLimitSplit {
            day_limit: base_limit,
            lysis_limit_minor,
        };
        let promis_limit_credit = base_limit
            .checked_sub(lysis_limit_minor)
            .ok_or_else(invalid)?;
        let desis_limit_minor = crate::settlement::desis_limit(
            nominal_total,
            lysis_limit_minor,
            base_limit,
            promis_limit_before,
            green,
        )
        .ok_or_else(invalid)?;
        Self::assemble(
            base_limit,
            lysis_limit_minor,
            desis_limit_minor,
            promis_limit_credit,
        )
    }

    fn assemble(
        base_limit: U256,
        lysis_limit_minor: U256,
        desis_limit_minor: U256,
        promis_limit_credit: U256,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidLysisLimitSplit {
            day_limit: base_limit,
            lysis_limit_minor,
        };
        if lysis_limit_minor.checked_add(promis_limit_credit) != Some(base_limit) {
            return Err(invalid().into());
        }
        let day_limit = base_limit
            .checked_add(desis_limit_minor)
            .ok_or_else(invalid)?;
        Ok(Self {
            day_limit,
            lysis_limit_minor,
            desis_limit_minor,
            promis_limit_credit,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LysisLimitInput {
    pub base_limit: U256,
    pub lysis_limit_minor: U256,
    pub nominal_total: U256,
    pub green: bool,
}

/// Reserve the successful day's allocation within the owning command checkpoint.
pub(crate) fn apply_lysis_limit(
    storage: StorageHandle<'_>,
    input: LysisLimitInput,
) -> Result<LimitSplit> {
    let carry_before = PromisLimitContract::new(storage.clone()).promis_limit()?;
    let split = LimitSplit::derive(
        input.base_limit,
        input.lysis_limit_minor,
        input.nominal_total,
        carry_before,
        input.green,
    )?;
    if !split.promis_limit_credit.is_zero() {
        let delta =
            PromisLimitContract::new(storage.clone()).checked_credit(split.promis_limit_credit)?;
        if delta.credited != split.promis_limit_credit {
            return Err(MetadosisError::LysisLimitCreditMismatch.into());
        }
    }
    if !split.desis_limit_minor.is_zero() {
        let mut promis = PromisLimitContract::new(storage);
        if promis.checked_take(split.desis_limit_minor)?.is_none() {
            return Err(MetadosisError::DesisLimitUnavailable {
                desis_limit_minor: split.desis_limit_minor,
                available: promis.promis_limit()?,
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
    green: bool,
    logical_anchor: u64,
) -> Result<()> {
    storage.with_checkpoint(|| {
        outbe_desis::api::dispatch_auction_brief(
            storage.clone(),
            outbe_desis::api::AuctionBrief {
                worldwide_day,
                desis_limit_minor: split.desis_limit_minor,
                is_green: green,
            },
            logical_anchor,
            outbe_desis::api::BriefOverflowPolicy::Reject,
        )
        .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    #[test]
    fn native_split_preserves_green_red_and_carry_economic_vectors() {
        for (base, lysis, nominal, carry, green, auction, remaining) in [
            (1_000, 32, 100, 0, true, 68, 900),
            (1_000, 320, 5_000, 0, true, 680, 0),
            (1_000, 320, 5_000, 2_000, true, 2_680, 0),
            (1_000, 4, 100, 7, false, 0, 1_003),
            (100, 40, 100, 5, true, 60, 5),
            (100, 40, 100, 5, false, 0, 65),
        ] {
            let mut provider = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut provider, |storage| {
                PromisLimitContract::new(storage.clone())
                    .checked_credit(U256::from(carry as u64))
                    .unwrap();
                let split = apply_lysis_limit(
                    storage.clone(),
                    LysisLimitInput {
                        base_limit: U256::from(base as u64),
                        lysis_limit_minor: U256::from(lysis as u64),
                        nominal_total: U256::from(nominal as u64),
                        green,
                    },
                )
                .unwrap();
                assert_eq!(split.desis_limit_minor, U256::from(auction as u64));
                assert_eq!(
                    PromisLimitContract::new(storage.clone())
                        .promis_limit()
                        .unwrap(),
                    U256::from(remaining as u64)
                );
                let day = WorldwideDay::new(20260101);
                apply_auction_brief(storage.clone(), day, &split, green, 1_699_920_005).unwrap();
                let desis = outbe_desis::DesisContract::new(storage.clone());
                assert_eq!(
                    desis.pending_desis_limit_minor.read(&day).unwrap(),
                    U256::from(auction as u64)
                );
                assert_eq!(desis.brief_green.read(&day).unwrap(), u8::from(green));
                assert_eq!(
                    PromisLimitContract::new(storage).promis_limit().unwrap(),
                    U256::from(remaining as u64)
                );
            });
        }
    }

    #[test]
    fn native_split_and_auction_failures_roll_back_the_whole_effect() {
        for (base, lysis, nominal, carry, green) in [
            (U256::MAX, U256::ZERO, U256::MAX, U256::ZERO, true),
            (
                U256::from(100),
                U256::from(40),
                U256::from(100),
                U256::MAX,
                false,
            ),
            (
                U256::from(u128::MAX) + U256::from(3),
                U256::ONE,
                U256::MAX,
                U256::ZERO,
                true,
            ),
        ] {
            let mut provider = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut provider, |storage| {
                PromisLimitContract::new(storage)
                    .checked_credit(carry)
                    .unwrap();
            });
            let before = provider.storage.clone();
            let events = provider.get_ordered_events().to_vec();
            let result = StorageHandle::enter(&mut provider, |storage| {
                storage.with_checkpoint(|| {
                    let split = apply_lysis_limit(
                        storage.clone(),
                        LysisLimitInput {
                            base_limit: base,
                            lysis_limit_minor: lysis,
                            nominal_total: nominal,
                            green,
                        },
                    )?;
                    apply_auction_brief(
                        storage.clone(),
                        WorldwideDay::new(20260101),
                        &split,
                        green,
                        1_699_920_005,
                    )
                })
            });
            assert!(result.is_err());
            assert_eq!(provider.storage, before);
            assert_eq!(provider.get_ordered_events(), events);
        }
    }
}
