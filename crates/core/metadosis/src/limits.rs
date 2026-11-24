//! The Metadosis limit of a day and its split: the loads of the Nods, the credit of the
//! Promis Limit and the Desis Limit.

use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext, error::Result, storage::CycleLifecyclePurpose,
    storage::StorageHandle,
};
use outbe_promislimit::PromisLimitContract;
use outbe_system_common::metadosis_bindings::metadosis_cycle_allocation_binding;
use outbe_system_common::time::date_key_to_utc_timestamp;

use crate::errors::MetadosisError;
use crate::precompile::IMetadosis;
use crate::schema::MetadosisContract;

/// Records that Metadosis of `day` holds `amount`. The Cycle writes the amount to the record of `day`.
pub(crate) fn set_metadosis_limit(ctx: &BlockRuntimeContext, day: u32, amount: U256) -> Result<()> {
    let day_ts = date_key_to_utc_timestamp(day).map_err(crate::lifecycle::date_step_error)?;
    let binding =
        metadosis_cycle_allocation_binding(ctx.block.chain_id, ctx.block.block_number, day_ts);
    crate::commit::commit_transition::<CycleLifecyclePurpose, _>(
        ctx.storage.clone(),
        binding,
        |_| {
            MetadosisContract::new(ctx.storage.clone()).emit(IMetadosis::MetadosisLimitSet {
                emissionDay: day,
                metadosisLimitMinor: amount,
            })
        },
    )
}

/// The smaller of the nominal less the loads and the unused day limit plus the draw, or `None`
/// when the loads exceed either of them or the sum overflows.
pub(crate) fn desis_limit(
    nominal_total: U256,
    nod_loads_minor: U256,
    day_limit: U256,
    promis_limit_draw: U256,
) -> Option<U256> {
    let available = day_limit
        .checked_sub(nod_loads_minor)?
        .checked_add(promis_limit_draw)?;
    Some(nominal_total.checked_sub(nod_loads_minor)?.min(available))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LimitSplit {
    /// The day's own emission plus what it drew from the Promis Limit.
    pub day_limit: U256,
    /// The sum of the loads of the Nods of the day, in protocol units (1e6 per whole COEN).
    pub nod_loads_minor: U256,
    /// Maximum Desis capacity, not actual Intex issuance, in protocol units.
    pub desis_limit_minor: U256,
    /// What the loads leave of the day's own emission, added before the auction draws.
    pub promis_limit_add: U256,
}

impl LimitSplit {
    /// The day's own emission bounds the loads, and the Promis Limit receives what the loads
    /// leave. The auction then draws from the Promis Limit: no more than the nominal beyond
    /// the symbolic share, and no more than it holds.
    pub(crate) fn derive(
        base_limit: U256,
        nod_loads_minor: U256,
        nominal_total: U256,
        promis_limit_before: U256,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidMetadosisLimitSplit {
            day_limit: base_limit,
            nod_loads_minor,
        };
        let promis_limit_add = base_limit
            .checked_sub(nod_loads_minor)
            .ok_or_else(invalid)?;
        let desis_limit_minor = desis_limit(
            nominal_total,
            nod_loads_minor,
            base_limit,
            promis_limit_before,
        )
        .ok_or_else(invalid)?;
        Self::assemble(
            base_limit,
            nod_loads_minor,
            desis_limit_minor,
            promis_limit_add,
        )
    }

    fn assemble(
        base_limit: U256,
        nod_loads_minor: U256,
        desis_limit_minor: U256,
        promis_limit_add: U256,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidMetadosisLimitSplit {
            day_limit: base_limit,
            nod_loads_minor,
        };
        if nod_loads_minor.checked_add(promis_limit_add) != Some(base_limit) {
            return Err(invalid().into());
        }
        let day_limit = base_limit
            .checked_add(desis_limit_minor)
            .ok_or_else(invalid)?;
        Ok(Self {
            day_limit,
            nod_loads_minor,
            desis_limit_minor,
            promis_limit_add,
        })
    }
}

/// Reserve the successful day's allocation within the owning command checkpoint.
pub(crate) fn allocate_day_metadosis_limit(
    storage: StorageHandle<'_>,
    base_limit: U256,
    nod_loads_minor: U256,
    nominal_total: U256,
) -> Result<LimitSplit> {
    let carry_before = PromisLimitContract::new(storage.clone()).promis_limit()?;
    let split = LimitSplit::derive(base_limit, nod_loads_minor, nominal_total, carry_before)?;
    if !split.promis_limit_add.is_zero() {
        PromisLimitContract::new(storage.clone()).add(
            outbe_promislimit::LimitSource::MetadosisLimit,
            split.promis_limit_add,
        )?;
    }
    let mut promis = PromisLimitContract::new(storage);
    if !split.desis_limit_minor.is_zero()
        && !promis.subtract(
            outbe_promislimit::LimitTarget::DesisLimit,
            split.desis_limit_minor,
        )?
    {
        return Err(MetadosisError::DesisLimitUnavailable {
            desis_limit_minor: split.desis_limit_minor,
            available: promis.promis_limit()?,
        }
        .into());
    }
    Ok(split)
}

/// Briefs Desis and returns the Desis Limit the auction holds. A limit that Desis
/// returns goes back to the Promis Limit.
pub(crate) fn apply_desis_limit(
    storage: StorageHandle<'_>,
    emission_day: u32,
    split: &LimitSplit,
) -> Result<U256> {
    let is_active = !split.desis_limit_minor.is_zero();
    storage.with_checkpoint(|| {
        let receipt = outbe_desis::api::set_desis_limit(
            storage.clone(),
            outbe_desis::api::DesisLimit {
                emission_day: emission_day.into(),
                desis_limit_minor: split.desis_limit_minor,
                has_limit: is_active,
            },
        )?;
        match receipt {
            outbe_desis::api::DesisLimitReceipt::Accepted => Ok(split.desis_limit_minor),
            outbe_desis::api::DesisLimitReceipt::ReturnedToPromisLimit {
                desis_limit_minor,
                ..
            } => {
                PromisLimitContract::new(storage.clone()).add(
                    outbe_promislimit::LimitSource::DesisLimit,
                    desis_limit_minor,
                )?;
                Ok(U256::ZERO)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_system_common::time::EmissionDate;

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
                    .add(
                        outbe_promislimit::LimitSource::MetadosisLimit,
                        U256::from(carry as u64),
                    )
                    .unwrap();
                let split = allocate_day_metadosis_limit(storage.clone(), U256::from(base as u64), U256::from(lysis as u64), U256::from(nominal as u64))
                .unwrap();
                assert_eq!(split.desis_limit_minor, U256::from(auction as u64));
                assert_eq!(
                    PromisLimitContract::new(storage.clone())
                        .promis_limit()
                        .unwrap(),
                    U256::from(remaining as u64)
                );
                let day = EmissionDate::new(20260101);
                outbe_desis::api::schedule_auction(storage.clone(), day).unwrap();
                apply_desis_limit(storage.clone(), day.value(), &split).unwrap();
                let desis = outbe_desis::DesisContract::new(storage.clone());
                assert_eq!(
                    desis.pending_desis_limit_minor.read(&day).unwrap(),
                    U256::from(auction as u64)
                );
                assert_eq!(
                    desis.has_desis_limit.read(&day).unwrap(),
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
                    .add(outbe_promislimit::LimitSource::MetadosisLimit, carry)
                    .unwrap();
            });
            let before = provider.storage.clone();
            let events = provider.get_ordered_events().to_vec();
            let result = StorageHandle::enter(&mut provider, |storage| {
                storage.with_checkpoint(|| {
                    let split = allocate_day_metadosis_limit(storage.clone(), base, lysis, nominal)?;
                    apply_desis_limit(storage.clone(), 20260101, &split)
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
            let split = allocate_day_metadosis_limit(storage.clone(), base, U256::ONE, U256::MAX)
            .unwrap();
            assert!(split.desis_limit_minor > U256::from(u128::MAX));
            let day = EmissionDate::new(20260101);
            outbe_desis::api::schedule_auction(storage.clone(), day).unwrap();
            assert_eq!(
                apply_desis_limit(storage.clone(), day.value(), &split).unwrap(),
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
                    .cancel_reason
                    .read(&day)
                    .unwrap(),
                outbe_desis::constants::CANCELED_LIMIT_OVERFLOW
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
