use crate::{
    aggregate::WwdProjection,
    commit::commit_outer_transition,
    constants::MAX_RETAINED_WWDS,
    precompile::IMetadosis,
    reducer::{OuterWwdTransition, WwdAdvanceEdge},
    schema::{MetadosisContract, WorldwideDayEntryExt},
    terminal::{CapacityForfeitureReceipt, MissedOfferingReceipt},
};
use outbe_primitives::{block::BlockRuntimeContext, error::Result, time::WorldwideDay};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

pub(super) struct WwdEffect<'a, 'storage> {
    pub ctx: &'a BlockRuntimeContext<'storage>,

    pub current: &'a WwdProjection,
    pub transition: &'a OuterWwdTransition,
}

pub(super) fn apply_wwd_advance_edges(
    metadosis: &mut MetadosisContract<'_>,
    wwd: WorldwideDay,
    edges: &[WwdAdvanceEdge],
) -> Result<()> {
    for edge in edges {
        apply_wwd_advance_edge_effect(metadosis, wwd, *edge)?;
    }
    Ok(())
}

fn apply_wwd_advance_edge_effect(
    metadosis: &mut MetadosisContract<'_>,
    wwd: WorldwideDay,
    edge: WwdAdvanceEdge,
) -> Result<()> {
    match edge {
        WwdAdvanceEdge::ResolveForming => resolve_forming_snapshot(metadosis, wwd),
        WwdAdvanceEdge::OpenOffering => {
            TributeContract::new(metadosis.storage.clone()).unseal_day(wwd)
        }
        WwdAdvanceEdge::CloseOffering => {
            TributeContract::new(metadosis.storage.clone()).seal_day(wwd)
        }
        WwdAdvanceEdge::BecomeReady => Ok(()),
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
    commit_outer_transition(
        metadosis,
        effect.current.worldwide_day,
        effect.transition,
        effect.ctx.block.block_number,
    )?;
    metadosis.emit(capacity_forfeiture_event(&receipt))
}

fn prepare_capacity_forfeiture(
    metadosis: &MetadosisContract<'_>,
    effect: &WwdEffect<'_, '_>,
    retained_count: usize,
) -> Result<(u32, u32)> {
    if retained_count != MAX_RETAINED_WWDS {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::CapacityForfeitureRequiresExactRetainedAdmissionCap
                .into(),
        );
    }

    if metadosis
        .read_capacity_forfeiture_receipt(effect.current.worldwide_day)?
        .is_some()
    {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::ActiveCapacityForfeitureVictimAlreadyHasReceipt
                .into(),
        );
    }
    metadosis.validate_day_limit_binding(effect.current, "CapacityForfeiture")?;

    let max_retained_wwds = u32::try_from(MAX_RETAINED_WWDS)
        .map_err(|_| crate::errors::MetadosisStorageCorruptionError::RetainedWwdCapExceedsU32)?;
    let retained_count_before = u32::try_from(retained_count)
        .map_err(|_| crate::errors::MetadosisStorageCorruptionError::RetainedWwdCountExceedsU32)?;
    Ok((max_retained_wwds, retained_count_before))
}

fn forfeit_and_credit_capacity(
    storage: outbe_primitives::storage::StorageHandle<'_>,
    effect: &WwdEffect<'_, '_>,
    (max_retained_wwds, retained_count_before): (u32, u32),
) -> Result<CapacityForfeitureReceipt> {
    let tribute = TributeContract::new(storage.clone())
        .forfeit_sealed_partition(effect.current.worldwide_day)?;
    let credit = PromisLimitContract::new(storage.clone())
        .checked_add_carry_over(effect.current.metadosis_limit_minor)?;
    Ok(CapacityForfeitureReceipt {
        worldwide_day: effect.current.worldwide_day,
        max_retained_wwds,
        retained_count_before,
        value_routed: credit.credited,
        carry_over_before: credit.before,
        carry_over_after: credit.after,
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
        current,
        transition,
    } = effect;

    metadosis.validate_day_limit_binding(current, "MissedOffering")?;
    if metadosis
        .read_missed_offering_receipt(current.worldwide_day)?
        .is_some()
    {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::ActiveWwdAlreadyHasTerminalReceipt
                .into(),
        );
    }

    let storage = metadosis.storage.clone();
    let result = (|| {
        let credit = PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(current.metadosis_limit_minor)?;
        let retirement = TributeContract::new(storage.clone())
            .retire_empty_missed_offering_partition(current.worldwide_day)?;
        let receipt = MissedOfferingReceipt {
            worldwide_day: current.worldwide_day,
            value_routed: credit.credited,
            carry_over_before: credit.before,
            carry_over_after: credit.after,
            retirement,
            block_number: ctx.block.block_number,
        };
        metadosis.write_missed_offering_receipt(receipt)?;
        commit_outer_transition(
            metadosis,
            current.worldwide_day,
            transition,
            ctx.block.block_number,
        )?;
        metadosis.emit(IMetadosis::WorldwideDayMissedOffering {
            worldwideDay: current.worldwide_day.into(),
            promisLimitReturnedMinor: receipt.value_routed,
            promisLimitBeforeMinor: receipt.carry_over_before,
            promisLimitAfterMinor: receipt.carry_over_after,
            retirementOutcome: match retirement {
                outbe_tribute::runtime::RetirementOutcome::NotPresent => 1,
                outbe_tribute::runtime::RetirementOutcome::Requested => 2,
            },
            blockNumber: receipt.block_number,
        })?;
        Ok(())
    })();
    result
}

use crate::errors::is_vwap_overflow;

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

/// Stores the day's VWAP snapshot at the end of Forming. Tribute pricing reads it.
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
