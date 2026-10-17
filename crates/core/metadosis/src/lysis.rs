//! Executes one sealed day against the current EVM journal.

use std::collections::{BTreeMap, BTreeSet};

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    tribute_encryption::EncryptedTributeV2,
};
use outbe_promislimit::PromisLimitContract;
use outbe_tee::{
    lysis::{LysisOutcome, LysisRequest},
    protocol::{FidelityCohortOp, FidelitySnapshotEntry, FidelitySnapshotRequest},
};
use outbe_tribute::TributeContract;

use crate::{
    aggregate::{WwdDayType, WwdProjection},
    commit::commit_outer_transition,
    limits::{apply_auction_brief, apply_lysis_limit, LysisLimitInput},
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent, ReadyDisposition},
    schema::MetadosisContract,
    settlement::MetadosisCalculation,
    terminal::model::{LysisCompletionDetail, TerminalReceiptCommon, WwdTerminalReceipt},
};

fn mismatch() -> PrecompileError {
    PrecompileError::fatal("on-chain Lysis input or result mismatch")
}

struct SettlementCompletion {
    tribute_count: u32,
    total_nominal_minor: alloy_primitives::U256,
    used: alloy_primitives::U256,
    unused: alloy_primitives::U256,
    inputs_hash: alloy_primitives::B256,
    result_hash: alloy_primitives::B256,
    retirement: outbe_tribute::runtime::RetirementOutcome,
}

fn validate_encrypted_tribute(
    ctx: &BlockRuntimeContext<'_>,
    owners: &mut BTreeSet<alloy_primitives::Address>,
    encrypted: &EncryptedTributeV2,
) -> Result<()> {
    let source = &encrypted.context;
    let valid_source = source.chain_id == ctx.block.chain_id && encrypted.has_valid_encoding();
    let unique_owner = owners.insert(source.owner)
        && outbe_nod::api::get_item(&ctx.storage, source.tribute_id)?.is_none();
    if valid_source && unique_owner {
        Ok(())
    } else {
        Err(mismatch())
    }
}

fn validate_day_records(
    ctx: &BlockRuntimeContext<'_>,
    tribute: &TributeContract<'_>,
    day: outbe_primitives::time::WorldwideDay,
    expected_count: u32,
) -> Result<Vec<EncryptedTributeV2>> {
    let records = tribute.state_records_by_day(day)?;
    if records.len() != expected_count as usize {
        return Err(mismatch());
    }
    let mut owners = BTreeSet::new();
    let mut tributes = Vec::with_capacity(records.len());
    for record in records {
        let encrypted: EncryptedTributeV2 = record.into();
        validate_encrypted_tribute(ctx, &mut owners, &encrypted)?;
        tributes.push(encrypted);
    }
    Ok(tributes)
}

fn resolve_green_day(day_type: WwdDayType) -> Result<bool> {
    match day_type {
        WwdDayType::Green => Ok(true),
        WwdDayType::Red => Ok(false),
        WwdDayType::Unknown => Err(mismatch()),
    }
}

fn finalize_lysis_settlement(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
    calculation: MetadosisCalculation,
    completion: SettlementCompletion,
) -> Result<()> {
    let storage = ctx.storage.clone();
    let day = current.worldwide_day;
    let carry_before = PromisLimitContract::new(storage.clone()).get_total_unallocated()?;
    let green = resolve_green_day(current.day_type)?;
    let split = apply_lysis_limit(
        storage.clone(),
        LysisLimitInput {
            base_limit: current.metadosis_limit_minor,
            lysis_limit_minor: calculation.lysis_limit_minor,
            nominal_total: completion.total_nominal_minor,
            green,
        },
    )?;
    if completion
        .used
        .checked_add(split.desis_limit_minor)
        .is_none_or(|allocation| allocation > completion.total_nominal_minor)
    {
        return Err(mismatch());
    }
    let credit =
        PromisLimitContract::new(storage.clone()).checked_add_carry_over(completion.unused)?;
    if credit.credited != completion.unused {
        return Err(mismatch());
    }
    apply_auction_brief(storage.clone(), day, &split, green, ctx.block.timestamp)?;
    let carry_after = PromisLimitContract::new(storage).get_total_unallocated()?;
    let transition = reduce_outer_wwd(
        Some(current),
        OuterWwdEvent::ProcessReady(ReadyDisposition::LysisCompleted),
    )?;
    metadosis.write_terminal_receipt(WwdTerminalReceipt::LysisCompleted {
        common: TerminalReceiptCommon {
            worldwide_day: day,
            value_routed: current.metadosis_limit_minor,
            carry_over_before: carry_before,
            carry_over_after: carry_after,
            retirement: completion.retirement,
            block_number: ctx.block.block_number,
        },
        detail: LysisCompletionDetail {
            tribute_count: completion.tribute_count,
            lysis_limit: calculation.lysis_limit_minor,
            used: completion.used,
            unused: completion.unused,
            desis_limit: split.desis_limit_minor,
            inputs_hash: completion.inputs_hash,
            result_hash: completion.result_hash,
        },
    })?;
    commit_outer_transition(metadosis, day, &transition, ctx.block.block_number)?;
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: day.into(),
        tributeNominalTotalMinor: completion.total_nominal_minor,
        gratisDemandMinor: calculation.gratis_demand,
        dayGratisLimitMinor: calculation.day_gratis_limit_minor,
        lysisLimitMinor: calculation.lysis_limit_minor,
        unusedLysisLimitMinor: completion.unused,
        lysisAllocationMinor: completion.used,
        promisLimitReturnedMinor: completion.unused,
        status: "COMPLETED".into(),
        blockNumber: ctx.block.block_number,
    })
}

fn load_ready_tribute<'a>(
    ctx: &BlockRuntimeContext<'a>,
    day: outbe_primitives::time::WorldwideDay,
) -> Result<(TributeContract<'a>, outbe_tribute::schema::DayTotals)> {
    let tribute = TributeContract::new(ctx.storage.clone());
    let totals = tribute.get_day_totals(day)?;
    if totals.is_sealed && totals.initialized && totals.tribute_count > 0 {
        Ok((tribute, totals))
    } else {
        Err(mismatch())
    }
}

fn execute_onchain_fractions(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
    calculation: MetadosisCalculation,
) -> Result<()> {
    let day = current.worldwide_day;
    let (mut tribute, totals) = load_ready_tribute(ctx, day)?;
    let (_, onchain_allocated_minor) = tribute.compute_and_record_metadosis_fractions(
        day,
        calculation.lysis_limit_minor,
        ctx.block.timestamp,
    )?;
    outbe_fidelity::api::finalize_wwd_max_fidelity(ctx.storage.clone(), day, ctx.block.timestamp)?;

    let used = onchain_allocated_minor.min(calculation.lysis_limit_minor);
    let unused = calculation.lysis_limit_minor.saturating_sub(used);
    finalize_lysis_settlement(
        metadosis,
        ctx,
        current,
        calculation,
        SettlementCompletion {
            tribute_count: totals.tribute_count,
            total_nominal_minor: totals.tribute_nominal_total_minor,
            used,
            unused,
            inputs_hash: alloy_primitives::keccak256(day.value().to_be_bytes()),
            result_hash: alloy_primitives::keccak256(used.to_be_bytes::<32>()),
            retirement: outbe_tribute::runtime::RetirementOutcome::NotPresent,
        },
    )
}

fn build_enclave_lysis_request(
    ctx: &BlockRuntimeContext<'_>,
    day: outbe_primitives::time::WorldwideDay,
    lysis_limit_minor: alloy_primitives::U256,
    tributes: Vec<EncryptedTributeV2>,
) -> Result<LysisRequest> {
    let mut currencies = BTreeMap::new();
    let mut fidelity = FidelitySnapshotRequest {
        timestamp: ctx.block.timestamp,
        first_qualified_start: 0,
        entries: Vec::with_capacity(tributes.len()),
    };
    let price_day = outbe_nod::openings::entry_price_day(ctx.block.timestamp);
    for encrypted in &tributes {
        let source = &encrypted.context;
        let section = outbe_fidelity::api::cohort_section(
            ctx.storage.clone(),
            source.owner,
            FidelityCohortOp::Probe,
            ctx.block.timestamp,
        )?;
        if !fidelity.entries.is_empty()
            && fidelity.first_qualified_start != section.first_qualified_start
        {
            return Err(mismatch());
        }
        fidelity.first_qualified_start = section.first_qualified_start;
        fidelity.entries.push(FidelitySnapshotEntry {
            owner: source.owner,
            cohort_blob: section.current_blob,
        });
        if let std::collections::btree_map::Entry::Vacant(entry) =
            currencies.entry(source.reference_currency)
        {
            entry.insert(outbe_oracle::api::get_utc_day_vwap_for_iso(
                ctx.storage.clone(),
                price_day,
                source.reference_currency,
            )?);
        }
    }
    Ok(LysisRequest {
        chain_id: ctx.block.chain_id,
        worldwide_day: day,
        timestamp: ctx.block.timestamp,
        lysis_limit_minor,
        tributes,
        fidelity,
        entry_prices: currencies.into_iter().collect(),
    })
}

fn consume_legacy_nods(
    ctx: &BlockRuntimeContext<'_>,
    tribute: &mut TributeContract<'_>,
    day: outbe_primitives::time::WorldwideDay,
    tribute_count: u32,
    result: &outbe_tee::lysis::VerifiedLysisResult,
) -> Result<outbe_tribute::runtime::RetirementOutcome> {
    for nod in &result.nods {
        outbe_nodfactory::api::issue_lysis_nod(&ctx.storage, nod)?;
    }
    let receipt = tribute.consume_and_retire_lysis_partition(
        day,
        tribute_count,
        result.total_nominal_minor,
    )?;
    if receipt.source_generation == 0 && receipt.retired_generation == 1 {
        Ok(receipt.retirement_outcome)
    } else {
        Err(mismatch())
    }
}

fn execute_legacy_enclave(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
    calculation: MetadosisCalculation,
) -> Result<()> {
    let day = current.worldwide_day;
    let (mut tribute, totals) = load_ready_tribute(ctx, day)?;
    let tributes = validate_day_records(ctx, &tribute, day, totals.tribute_count)?;
    let request = build_enclave_lysis_request(ctx, day, calculation.lysis_limit_minor, tributes)?;
    let outcome = outbe_tee::lysis::execute(request)
        .map_err(|_| PrecompileError::EnclaveUnavailable("on-chain Lysis request failed".into()))?;
    let LysisOutcome::Completed(result) = outcome else {
        let transition = reduce_outer_wwd(
            Some(current),
            OuterWwdEvent::ProcessReady(ReadyDisposition::LysisFailed),
        )?;
        commit_outer_transition(metadosis, day, &transition, ctx.block.block_number)?;
        return metadosis.emit(IMetadosis::WorldwideDayStatusChange {
            worldwideDay: day.into(),
            oldStatus: current.status.as_u8(),
            newStatus: transition.target().as_u8(),
            blockNumber: ctx.block.block_number,
        });
    };
    if result.total_nominal_minor != totals.tribute_nominal_total_minor {
        return Err(mismatch());
    }
    tribute.compute_and_record_metadosis_fractions(
        day,
        calculation.lysis_limit_minor,
        ctx.block.timestamp,
    )?;
    outbe_fidelity::api::finalize_wwd_max_fidelity(ctx.storage.clone(), day, ctx.block.timestamp)?;
    let used = calculation
        .lysis_limit_minor
        .checked_sub(result.unused_lysis_limit_minor)
        .ok_or_else(mismatch)?;
    let retirement = consume_legacy_nods(ctx, &mut tribute, day, totals.tribute_count, &result)?;
    finalize_lysis_settlement(
        metadosis,
        ctx,
        current,
        calculation,
        SettlementCompletion {
            tribute_count: totals.tribute_count,
            total_nominal_minor: result.total_nominal_minor,
            used,
            unused: result.unused_lysis_limit_minor,
            inputs_hash: result.inputs_hash,
            result_hash: result.result_hash,
            retirement,
        },
    )
}

pub(crate) fn execute(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
    calculation: MetadosisCalculation,
) -> Result<()> {
    ctx.storage.require_metadosis_process_ready()?;
    if outbe_tee::lysis::is_test_transport_installed() {
        execute_legacy_enclave(metadosis, ctx, current, calculation)
    } else {
        execute_onchain_fractions(metadosis, ctx, current, calculation)
    }
}
