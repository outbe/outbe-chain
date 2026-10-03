use super::IMetadosis;
use crate::{
    aggregate::{WwdDayType, WwdStatus},
    schema::{terminal_outcome, terminal_retirement, MetadosisContract, WorldwideDayEntryExt},
};
use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

pub(super) fn worldwide_day(
    metadosis: &MetadosisContract<'_>,
    c: IMetadosis::getWorldwideDayCall,
) -> Result<<IMetadosis::getWorldwideDayCall as SolCall>::Return> {
    let Some(day) = metadosis.worldwide_days.get(c.wwd.into())? else {
        return Err(outbe_primitives::storage::dsl::missing_record_err(
            "WorldwideDay",
        ));
    };
    // Persisted bytes are validated as closed tags before they are
    // returned through the raw ABI representation.
    WwdStatus::try_from(day.status)?;
    WwdDayType::try_from(day.day_type)?;
    Ok((
        day.status,
        day.day_type,
        day.forming_start,
        day.forming_end,
        day.lookback_end,
        day.offering_end,
        day.scheduled_process_time,
        day.previous_vwap,
        day.current_vwap,
    )
        .into())
}

pub(super) fn worldwide_days_by_status(
    metadosis: &MetadosisContract<'_>,
    c: IMetadosis::getWorldwideDaysByStatusCall,
) -> Result<<IMetadosis::getWorldwideDaysByStatusCall as SolCall>::Return> {
    let wanted = WwdStatus::try_from(c.status)
        .map_err(|_| crate::errors::caller_rejection("unknown WorldwideDay status"))?;
    let wwds = metadosis.get_active_wwd_by_status(wanted)?;
    Ok(wwds.into_iter().map(u32::from).collect())
}

pub(super) fn terminal_receipt(
    metadosis: &MetadosisContract<'_>,
    c: IMetadosis::getWorldwideDayTerminalReceiptCall,
) -> Result<<IMetadosis::getWorldwideDayTerminalReceiptCall as SolCall>::Return> {
    let wwd = c.wwd.into();
    let Some(stored) = metadosis.read_terminal_receipt(wwd)? else {
        return Ok((
            terminal_outcome::NONE,
            U256::ZERO,
            U256::ZERO,
            U256::ZERO,
            terminal_retirement::NONE,
            0_u64,
        )
            .into());
    };
    match &stored {
        crate::terminal::model::WwdTerminalReceipt::MissedOffering(_) => {
            metadosis.read_missed_offering_receipt(wwd)?;
        }
        crate::terminal::model::WwdTerminalReceipt::CapacityForfeiture { .. } => {
            metadosis.read_capacity_forfeiture_receipt(wwd)?;
        }
        crate::terminal::model::WwdTerminalReceipt::MetadosisFailure(_) => {
            let expected_value_routed = metadosis
                .request_limit_receipt(wwd, &crate::ocomp::schema::poc_schema_limits())?
                .map_or(
                    metadosis
                        .worldwide_days
                        .entry(wwd)
                        .metadosis_limit_minor()
                        .read()?,
                    |receipt| receipt.lysis_limit_minor,
                );
            metadosis.read_metadosis_failure_receipt(wwd, expected_value_routed)?;
        }
    }
    let common = stored.common();
    Ok((
        stored.outcome(),
        common.value_routed,
        common.carry_over_before,
        common.carry_over_after,
        crate::terminal::encode_retirement(common.retirement),
        common.block_number,
    )
        .into())
}

pub(super) fn capacity_forfeiture_receipt(
    metadosis: &MetadosisContract<'_>,
    c: IMetadosis::getCapacityForfeitureReceiptCall,
) -> Result<<IMetadosis::getCapacityForfeitureReceiptCall as SolCall>::Return> {
    let Some(receipt) = metadosis.read_capacity_forfeiture_receipt(c.wwd.into())? else {
        return Ok((
            terminal_outcome::NONE,
            0_u32,
            0_u32,
            U256::ZERO,
            U256::ZERO,
            U256::ZERO,
            alloy_primitives::B256::ZERO,
            0_u32,
            U256::ZERO,
            0_u64,
            0_u64,
            terminal_retirement::NONE,
            0_u64,
        )
            .into());
    };
    let retirement = match receipt.retirement {
        outbe_compressed_entities::RetirementOutcome::NotPresent => {
            terminal_retirement::NOT_PRESENT
        }
        outbe_compressed_entities::RetirementOutcome::Requested => terminal_retirement::REQUESTED,
    };
    Ok((
        terminal_outcome::CAPACITY_FORFEITURE,
        receipt.max_retained_wwds,
        receipt.retained_count_before,
        receipt.value_routed,
        receipt.carry_over_before,
        receipt.carry_over_after,
        receipt.sealed_collection_root,
        receipt.forfeited_count,
        receipt.forfeited_nominal,
        receipt.source_generation,
        receipt.retired_generation,
        retirement,
        receipt.block_number,
    )
        .into())
}
