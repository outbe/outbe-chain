//! Native Lysis capacity allocation and auction dispatch.

use alloy_primitives::U256;
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_promislimit::PromisLimitContract;

use crate::errors::MetadosisError;

/// The smaller of nominal minus Lysis and the unused day limit plus the draw, or `None` when
/// Lysis exceeds either of them or the sum overflows.
pub(crate) fn desis_limit(
    nominal_total: U256,
    lysis_limit_minor: U256,
    day_limit: U256,
    promis_limit_draw: U256,
) -> Option<U256> {
    let available = day_limit
        .checked_sub(lysis_limit_minor)?
        .checked_add(promis_limit_draw)?;
    Some(nominal_total.checked_sub(lysis_limit_minor)?.min(available))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LimitSplit {
    /// The day's own emission plus what it drew from the Promis Limit.
    pub day_limit: U256,
    /// Maximum Lysis capacity, in protocol units (1e6 per whole COEN).
    pub lysis_limit_minor: U256,
    /// Maximum Desis capacity, not actual Intex issuance, in protocol units.
    pub desis_limit_minor: U256,
    /// What Lysis left of the day's own emission, credited before the auction draws.
    pub promis_limit_credit: U256,
}

impl LimitSplit {
    /// The day's own emission bounds Lysis, and the Promis Limit receives the credit for what
    /// Lysis leaves. The auction then draws from the Promis Limit: no more than the nominal beyond
    /// the symbolic share, and no more than it holds.
    pub(crate) fn derive(
        base_limit: U256,
        lysis_limit_minor: U256,
        nominal_total: U256,
        promis_limit_before: U256,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidLysisLimitSplit {
            day_limit: base_limit,
            lysis_limit_minor,
        };
        let promis_limit_credit = base_limit
            .checked_sub(lysis_limit_minor)
            .ok_or_else(invalid)?;
        let desis_limit_minor = desis_limit(
            nominal_total,
            lysis_limit_minor,
            base_limit,
            promis_limit_before,
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
    /// What the Nods leave of the Lysis Limit; it goes back to the Promis Limit.
    pub unused_lysis_minor: U256,
}

/// Reserve the successful day's allocation within the owning command checkpoint.
pub(crate) fn allocate_day_metadosis_limit(
    storage: StorageHandle<'_>,
    input: LysisLimitInput,
) -> Result<LimitSplit> {
    let carry_before = PromisLimitContract::new(storage.clone()).promis_limit()?;
    let split = LimitSplit::derive(
        input.base_limit,
        input.lysis_limit_minor,
        input.nominal_total,
        carry_before,
    )?;
    if !split.promis_limit_credit.is_zero() {
        PromisLimitContract::new(storage.clone()).checked_credit(split.promis_limit_credit)?;
    }
    let mut promis = PromisLimitContract::new(storage);
    if !split.desis_limit_minor.is_zero() && promis.checked_take(split.desis_limit_minor)?.is_none()
    {
        return Err(MetadosisError::DesisLimitUnavailable {
            desis_limit_minor: split.desis_limit_minor,
            available: promis.promis_limit()?,
        }
        .into());
    }
    promis.checked_credit(input.unused_lysis_minor)?;
    Ok(split)
}

/// Briefs Desis and returns the Desis Limit the auction holds. A limit that Desis
/// returns goes back to the Promis Limit.
pub(crate) fn brief_auction(
    storage: StorageHandle<'_>,
    emission_day: u32,
    split: &LimitSplit,
) -> Result<U256> {
    let is_active = !split.desis_limit_minor.is_zero();
    storage.with_checkpoint(|| {
        let receipt = outbe_desis::api::brief_auction(
            storage.clone(),
            outbe_desis::api::AuctionBrief {
                emission_day: emission_day.into(),
                desis_limit_minor: split.desis_limit_minor,
                has_limit: is_active,
            },
        )?;
        match receipt {
            outbe_desis::api::AuctionBriefReceipt::Accepted => Ok(split.desis_limit_minor),
            outbe_desis::api::AuctionBriefReceipt::ReturnedToPromisLimit {
                desis_limit_minor,
                ..
            } => {
                PromisLimitContract::new(storage.clone()).checked_credit(desis_limit_minor)?;
                Ok(U256::ZERO)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::time::EmissionDate;

    #[test]
    fn native_split_preserves_the_day_and_carry_economic_vectors() {
        for (base, lysis, nominal, carry, auction, remaining) in [
            (1_000, 32, 100, 0, 68, 900),
            (1_000, 320, 5_000, 0, 680, 0),
            (1_000, 320, 5_000, 2_000, 2_680, 0),
            (1_000, 4, 100, 7, 96, 907),
            (100, 40, 100, 5, 60, 5),
            (100, 100, 100, 5, 0, 5),
        ] {
            let mut provider = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut provider, |storage| {
                PromisLimitContract::new(storage.clone())
                    .checked_credit(U256::from(carry as u64))
                    .unwrap();
                let split = allocate_day_metadosis_limit(
                    storage.clone(),
                    LysisLimitInput {
                        base_limit: U256::from(base as u64),
                        lysis_limit_minor: U256::from(lysis as u64),
                        nominal_total: U256::from(nominal as u64),
                        unused_lysis_minor: U256::ZERO,
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
                let day = EmissionDate::new(20260101);
                brief_auction(storage.clone(), day.value(), &split).unwrap();
                let desis = outbe_desis::DesisContract::new(storage.clone());
                assert_eq!(
                    desis.pending_desis_limit_minor.read(&day).unwrap(),
                    U256::from(auction as u64)
                );
                assert_eq!(
                    desis.brief_has_limit.read(&day).unwrap(),
                    u8::from(auction != 0)
                );
                assert_eq!(
                    PromisLimitContract::new(storage).promis_limit().unwrap(),
                    U256::from(remaining as u64)
                );
            });
        }
    }

    #[test]
    fn native_split_and_auction_failures_roll_back_the_whole_effect() {
        for (base, lysis, nominal, carry) in [
            (U256::MAX, U256::ZERO, U256::MAX, U256::ZERO),
            (U256::from(100), U256::from(40), U256::from(100), U256::MAX),
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
                    let split = allocate_day_metadosis_limit(
                        storage.clone(),
                        LysisLimitInput {
                            base_limit: base,
                            lysis_limit_minor: lysis,
                            nominal_total: nominal,
                            unused_lysis_minor: U256::ZERO,
                        },
                    )?;
                    brief_auction(storage.clone(), 20260101, &split)
                })
            });
            assert!(result.is_err());
            assert_eq!(provider.storage, before);
            assert_eq!(provider.get_ordered_events(), events);
        }
    }

    #[test]
    fn a_desis_limit_above_the_auction_domain_goes_back_to_the_promis_limit() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let base = U256::from(u128::MAX) + U256::from(3);
            let split = allocate_day_metadosis_limit(
                storage.clone(),
                LysisLimitInput {
                    base_limit: base,
                    lysis_limit_minor: U256::ONE,
                    nominal_total: U256::MAX,
                    unused_lysis_minor: U256::ZERO,
                },
            )
            .unwrap();
            assert!(split.desis_limit_minor > U256::from(u128::MAX));
            let day = EmissionDate::new(20260101);
            assert_eq!(
                brief_auction(storage.clone(), day.value(), &split).unwrap(),
                U256::ZERO
            );
            assert_eq!(
                PromisLimitContract::new(storage.clone())
                    .promis_limit()
                    .unwrap(),
                base - U256::ONE
            );
            assert_eq!(
                outbe_desis::DesisContract::new(storage)
                    .auction_stage
                    .read(&day)
                    .unwrap(),
                outbe_desis::AuctionStage::Cancelled as u8
            );
        });
    }

    #[test]
    fn desis_limit_refuses_each_checked_arm_and_returns_the_minimum() {
        let cases = [
            (
                U256::from(1u64),
                U256::from(1u64),
                U256::ZERO,
                U256::ZERO,
                None,
            ),
            (U256::MAX, U256::ZERO, U256::MAX, U256::from(1u64), None),
            (
                U256::from(4u64),
                U256::from(5u64),
                U256::from(10u64),
                U256::ZERO,
                None,
            ),
            (
                U256::from(50u64),
                U256::from(10u64),
                U256::from(40u64),
                U256::from(5u64),
                Some(U256::from(35u64)),
            ),
            (
                U256::from(30u64),
                U256::from(10u64),
                U256::from(100u64),
                U256::ZERO,
                Some(U256::from(20u64)),
            ),
            (
                U256::MAX,
                U256::ZERO,
                U256::MAX,
                U256::ZERO,
                Some(U256::MAX),
            ),
        ];
        for (nominal, lysis, day_limit, draw, expected) in cases {
            assert_eq!(
                desis_limit(nominal, lysis, day_limit, draw),
                expected,
                "nominal {nominal} lysis {lysis} day_limit {day_limit} draw {draw}"
            );
        }
    }
}
