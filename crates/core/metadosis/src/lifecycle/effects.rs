use crate::{
    aggregate::{WwdDayType, WwdProjection},
    commit::{commit_outer_transition_with_rate, WwdRateResolution},
    constants::MAX_RETAINED_WWDS,
    precompile::IMetadosis,
    reducer::{OuterWwdTransition, WwdAdvanceEdge},
    schema::{MetadosisContract, WorldwideDayEntryExt},
    terminal::{CapacityForfeitureReceipt, MissedOfferingReceipt},
};
use alloy_primitives::U256;
use outbe_compressed_entities::ExecutionScope;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    time::WorldwideDay,
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

pub(super) struct WwdEffect<'a, 'storage> {
    pub ctx: &'a BlockRuntimeContext<'storage>,
    pub scope: &'a ExecutionScope,
    pub current: &'a WwdProjection,
    pub transition: &'a OuterWwdTransition,
    pub rate_resolution: Option<WwdRateResolution>,
}

pub(super) fn apply_wwd_advance_edges(
    metadosis: &mut MetadosisContract<'_>,
    wwd: WorldwideDay,
    edges: &[WwdAdvanceEdge],
    duplicate_resolution_message: &'static str,
) -> Result<Option<WwdRateResolution>> {
    let mut rate_resolution = None;
    for edge in edges {
        let edge_resolution = apply_wwd_advance_edge_effect(metadosis, wwd, *edge)?;
        let Some(resolution) = edge_resolution else {
            continue;
        };
        if rate_resolution.replace(resolution).is_some() {
            return Err(crate::errors::storage_corruption(
                duplicate_resolution_message.into(),
            ));
        }
    }
    Ok(rate_resolution)
}

fn apply_wwd_advance_edge_effect(
    metadosis: &mut MetadosisContract<'_>,
    wwd: WorldwideDay,
    edge: WwdAdvanceEdge,
) -> Result<Option<WwdRateResolution>> {
    match edge {
        WwdAdvanceEdge::ResolveForming => {
            resolve_forming_snapshot(metadosis, wwd)?;
            Ok(Some(resolve_day_rate(metadosis, wwd)?))
        }
        WwdAdvanceEdge::OpenOffering => {
            TributeContract::new(metadosis.storage.clone()).unseal_day(wwd)?;
            Ok(None)
        }
        WwdAdvanceEdge::CloseOffering => {
            TributeContract::new(metadosis.storage.clone()).seal_day(wwd)?;
            Ok(None)
        }
        WwdAdvanceEdge::BecomeReady => Ok(None),
    }
}

pub(super) fn apply_capacity_forfeiture(
    metadosis: &mut MetadosisContract<'_>,
    effect: WwdEffect<'_, '_>,
    retained_count: usize,
) -> Result<()> {
    let counts = prepare_capacity_forfeiture(metadosis, &effect, retained_count)?;
    let receipt = forfeit_and_credit_capacity(metadosis.storage.clone(), &effect, counts)?;
    metadosis.write_capacity_forfeiture_receipt(receipt)?;
    commit_outer_transition_with_rate(
        metadosis,
        effect.current.worldwide_day,
        effect.transition,
        effect.ctx.block.block_number,
        effect.rate_resolution,
    )?;
    metadosis.emit(capacity_forfeiture_event(&receipt))
}

fn prepare_capacity_forfeiture(
    metadosis: &MetadosisContract<'_>,
    effect: &WwdEffect<'_, '_>,
    retained_count: usize,
) -> Result<(u32, u32)> {
    if retained_count != MAX_RETAINED_WWDS {
        return Err(crate::errors::storage_corruption(
            "CapacityForfeiture requires the exact retained admission cap".into(),
        ));
    }
    if !metadosis
        .ocomp_fsm_states
        .get_bytes(&effect.current.worldwide_day)
        .is_empty()?
    {
        return Err(crate::errors::storage_corruption(
            "CapacityForfeiture victim already has OCOMP state".into(),
        ));
    }
    if metadosis
        .read_capacity_forfeiture_receipt(effect.current.worldwide_day)?
        .is_some()
    {
        return Err(crate::errors::storage_corruption(
            "active CapacityForfeiture victim already has a receipt".into(),
        ));
    }
    metadosis.validate_day_limit_binding(effect.current, "CapacityForfeiture")?;

    let max_retained_wwds = u32::try_from(MAX_RETAINED_WWDS)
        .map_err(|_| crate::errors::storage_corruption("retained WWD cap exceeds u32".into()))?;
    let retained_count_before = u32::try_from(retained_count)
        .map_err(|_| crate::errors::storage_corruption("retained WWD count exceeds u32".into()))?;
    Ok((max_retained_wwds, retained_count_before))
}

fn forfeit_and_credit_capacity(
    storage: outbe_primitives::storage::StorageHandle<'_>,
    effect: &WwdEffect<'_, '_>,
    (max_retained_wwds, retained_count_before): (u32, u32),
) -> Result<CapacityForfeitureReceipt> {
    let tribute = TributeContract::new(storage.clone())
        .forfeit_sealed_partition(effect.scope, effect.current.worldwide_day)?;
    let credit = PromisLimitContract::new(storage.clone())
        .checked_add_carry_over(effect.current.metadosis_limit_minor)?;
    Ok(CapacityForfeitureReceipt {
        worldwide_day: effect.current.worldwide_day,
        max_retained_wwds,
        retained_count_before,
        value_routed: credit.credited,
        carry_over_before: credit.before,
        carry_over_after: credit.after,
        sealed_collection_root: tribute.sealed_root,
        forfeited_count: tribute.forfeited_count,
        forfeited_nominal: tribute.forfeited_nominal,
        source_generation: tribute.source_generation,
        retired_generation: tribute.retired_generation,
        retirement: tribute.retirement_outcome,
        block_number: effect.ctx.block.block_number,
    })
}

fn capacity_forfeiture_event(
    receipt: &CapacityForfeitureReceipt,
) -> IMetadosis::WorldwideDayCapacityForfeited {
    IMetadosis::WorldwideDayCapacityForfeited {
        worldwideDay: receipt.worldwide_day.into(),
        maxRetainedWorldwideDays: receipt.max_retained_wwds,
        retainedCountBefore: receipt.retained_count_before,
        promisLimitReturnedMinor: receipt.value_routed,
        promisLimitBeforeMinor: receipt.carry_over_before,
        promisLimitAfterMinor: receipt.carry_over_after,
        sealedCollectionRoot: receipt.sealed_collection_root,
        forfeitedTributeCount: receipt.forfeited_count,
        forfeitedTributeNominalMinor: receipt.forfeited_nominal,
        sourceGeneration: receipt.source_generation,
        retiredGeneration: receipt.retired_generation,
        retirementOutcome: crate::terminal::encode_retirement(receipt.retirement),
        blockNumber: receipt.block_number,
    }
}

pub(super) fn apply_missed_offering(
    metadosis: &mut MetadosisContract<'_>,
    effect: WwdEffect<'_, '_>,
) -> Result<()> {
    let WwdEffect {
        ctx,
        scope,
        current,
        transition,
        rate_resolution,
    } = effect;

    metadosis.validate_day_limit_binding(current, "MissedOffering")?;
    if metadosis
        .read_missed_offering_receipt(current.worldwide_day)?
        .is_some()
    {
        return Err(crate::errors::storage_corruption(
            "active WWD already has a terminal receipt".into(),
        ));
    }

    let storage = metadosis.storage.clone();
    let result = (|| {
        let credit = PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(current.metadosis_limit_minor)?;
        let retirement = TributeContract::new(storage.clone())
            .retire_empty_missed_offering_partition(scope, current.worldwide_day)?;
        let receipt = MissedOfferingReceipt {
            worldwide_day: current.worldwide_day,
            value_routed: credit.credited,
            carry_over_before: credit.before,
            carry_over_after: credit.after,
            retirement,
            block_number: ctx.block.block_number,
        };
        metadosis.write_missed_offering_receipt(receipt)?;
        commit_outer_transition_with_rate(
            metadosis,
            current.worldwide_day,
            transition,
            ctx.block.block_number,
            rate_resolution,
        )?;
        metadosis.emit(IMetadosis::WorldwideDayMissedOffering {
            worldwideDay: current.worldwide_day.into(),
            promisLimitReturnedMinor: receipt.value_routed,
            promisLimitBeforeMinor: receipt.carry_over_before,
            promisLimitAfterMinor: receipt.carry_over_after,
            retirementOutcome: match retirement {
                outbe_compressed_entities::RetirementOutcome::NotPresent => 1,
                outbe_compressed_entities::RetirementOutcome::Requested => 2,
            },
            blockNumber: receipt.block_number,
        })?;
        Ok(())
    })();
    result
}

fn is_vwap_overflow(err: &PrecompileError) -> bool {
    matches!(err, PrecompileError::Revert(message) if message.starts_with("VWAP overflow:"))
}

fn store_worldwide_day_vwap_snapshot(
    metadosis: &mut MetadosisContract,
    wwd: WorldwideDay,
) -> Result<()> {
    let forming_start = metadosis.worldwide_days.entry(wwd).forming_start().read()?;
    let forming_end = metadosis.worldwide_days.entry(wwd).forming_end().read()?;
    outbe_oracle::api::store_worldwide_day_vwap_snapshot(
        metadosis.storage.clone(),
        wwd,
        forming_start,
        forming_end,
    )?;
    Ok(())
}

fn resolve_day_rate(metadosis: &MetadosisContract, wwd: WorldwideDay) -> Result<WwdRateResolution> {
    let current_vwap = outbe_oracle::api::day_type_pair_vwap(metadosis.storage.clone(), wwd)?
        .unwrap_or(U256::ZERO);
    let previous_vwap = if current_vwap.is_zero() {
        U256::ZERO
    } else {
        outbe_oracle::api::day_type_pair_vwap(metadosis.storage.clone(), wwd.previous_date_key())?
            .unwrap_or(U256::ZERO)
    };
    Ok(WwdRateResolution {
        previous_vwap,
        current_vwap,
        day_type: determine_day_type(previous_vwap, current_vwap),
    })
}

fn determine_day_type(previous_vwap: U256, current_vwap: U256) -> WwdDayType {
    if previous_vwap.is_zero() || current_vwap.is_zero() {
        return WwdDayType::Red;
    }
    if current_vwap > previous_vwap {
        WwdDayType::Green
    } else {
        WwdDayType::Red
    }
}
fn resolve_forming_snapshot(
    metadosis: &mut MetadosisContract<'_>,
    wwd: WorldwideDay,
) -> Result<()> {
    let Err(err) = store_worldwide_day_vwap_snapshot(metadosis, wwd) else {
        return Ok(());
    };
    if !is_vwap_overflow(&err) {
        return Err(err);
    }
    tracing::error!(target: "outbe::cycle", worldwide_day = %wwd, error = %err, "worldwide day VWAP calculation failed");
    Ok(())
}
