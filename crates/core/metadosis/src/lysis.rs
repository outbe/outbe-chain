//! Executes one sealed WorldwideDay linked to a due EmissionDay.

use std::collections::{BTreeMap, BTreeSet};

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    time::WorldwideDay,
    tribute_encryption::EncryptedTributeV2,
};
use outbe_promislimit::PromisLimitContract;
use outbe_tee::{
    lysis::{LysisOutcome, LysisRequest},
    protocol::{FidelityCohortOp, FidelitySnapshotEntry, FidelitySnapshotRequest},
};
use outbe_tribute::TributeContract;

use crate::{
    lifecycle::complete_emission_day,
    limits::{apply_auction_brief, apply_lysis_limit, LysisLimitInput},
    precompile::IMetadosis,
    schema::{emission_day_state, EmissionDay, MetadosisContract},
    settlement::MetadosisCalculation,
};

fn mismatch() -> PrecompileError {
    PrecompileError::fatal("on-chain Lysis input or result mismatch")
}

struct SettlementCompletion {
    total_nominal_minor: alloy_primitives::U256,
    used: alloy_primitives::U256,
    unused: alloy_primitives::U256,
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
    day: WorldwideDay,
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

fn finalize_lysis_settlement(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
    calculation: MetadosisCalculation,
    completion: SettlementCompletion,
) -> Result<()> {
    let storage = ctx.storage.clone();
    let wwd = emission_day.wwd;
    let split = apply_lysis_limit(
        storage.clone(),
        LysisLimitInput {
            base_limit: emission_day.emission_limit_minor,
            lysis_limit_minor: calculation.lysis_limit_minor,
            nominal_total: completion.total_nominal_minor,
        },
    )?;
    if completion
        .used
        .checked_add(split.desis_limit_minor)
        .is_none_or(|allocation| allocation > completion.total_nominal_minor)
    {
        return Err(mismatch());
    }
    let credit = PromisLimitContract::new(storage.clone()).checked_credit(completion.unused)?;
    if credit.credited != completion.unused {
        return Err(mismatch());
    }
    apply_auction_brief(storage, wwd, &split, ctx.block.timestamp)?;
    complete_emission_day(metadosis, emission_day, emission_day_state::SUCCEEDED)?;
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: wwd.into(),
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
    day: WorldwideDay,
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
    emission_day: &EmissionDay,
    calculation: MetadosisCalculation,
) -> Result<()> {
    let wwd = emission_day.wwd;
    let (mut tribute, totals) = load_ready_tribute(ctx, wwd)?;
    tribute.freeze_day_nominal(&totals)?;
    let (_, onchain_allocated_minor) = tribute.compute_and_record_metadosis_fractions(
        wwd,
        calculation.lysis_limit_minor,
        ctx.block.timestamp,
    )?;
    outbe_fidelity::api::finalize_wwd_max_fidelity(ctx.storage.clone(), wwd, ctx.block.timestamp)?;

    let used = onchain_allocated_minor.min(calculation.lysis_limit_minor);
    let unused = calculation.lysis_limit_minor.saturating_sub(used);
    finalize_lysis_settlement(
        metadosis,
        ctx,
        emission_day,
        calculation,
        SettlementCompletion {
            total_nominal_minor: totals.tribute_nominal_total_minor,
            used,
            unused,
        },
    )
}

fn build_enclave_lysis_request(
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
    lysis_limit_minor: alloy_primitives::U256,
    tributes: Vec<EncryptedTributeV2>,
) -> Result<LysisRequest> {
    let mut currencies = BTreeMap::new();
    let mut fidelity = FidelitySnapshotRequest {
        timestamp: ctx.block.timestamp,
        first_qualified_start: 0,
        entries: Vec::with_capacity(tributes.len()),
    };
    // Entry price uses the 24h UTC VWAP of `emission_day.day` (e.g. 15/10/2026),
    // while tribute nominals and S-curve belong to `emission_day.wwd` (e.g. 20/09/2026).
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
                emission_day.day,
                source.reference_currency,
            )?);
        }
    }
    Ok(LysisRequest {
        chain_id: ctx.block.chain_id,
        worldwide_day: emission_day.wwd,
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
    day: WorldwideDay,
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
    emission_day: &EmissionDay,
    calculation: MetadosisCalculation,
) -> Result<()> {
    let wwd = emission_day.wwd;
    let (mut tribute, totals) = load_ready_tribute(ctx, wwd)?;
    let tributes = validate_day_records(ctx, &tribute, wwd, totals.tribute_count)?;
    let request =
        build_enclave_lysis_request(ctx, emission_day, calculation.lysis_limit_minor, tributes)?;
    let outcome = outbe_tee::lysis::execute(request)
        .map_err(|_| PrecompileError::EnclaveUnavailable("on-chain Lysis request failed".into()))?;
    let LysisOutcome::Completed(result) = outcome else {
        return complete_emission_day(metadosis, emission_day, emission_day_state::FAILED);
    };
    if result.total_nominal_minor != totals.tribute_nominal_total_minor {
        return Err(mismatch());
    }
    tribute.compute_and_record_metadosis_fractions(
        wwd,
        calculation.lysis_limit_minor,
        ctx.block.timestamp,
    )?;
    outbe_fidelity::api::finalize_wwd_max_fidelity(ctx.storage.clone(), wwd, ctx.block.timestamp)?;
    let used = calculation
        .lysis_limit_minor
        .checked_sub(result.unused_lysis_limit_minor)
        .ok_or_else(mismatch)?;
    let _ = consume_legacy_nods(ctx, &mut tribute, wwd, totals.tribute_count, &result)?;
    finalize_lysis_settlement(
        metadosis,
        ctx,
        emission_day,
        calculation,
        SettlementCompletion {
            total_nominal_minor: result.total_nominal_minor,
            used,
            unused: result.unused_lysis_limit_minor,
        },
    )
}

pub(crate) fn execute(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
    calculation: MetadosisCalculation,
) -> Result<()> {
    ctx.storage.require_metadosis_process_ready()?;
    if outbe_tee::lysis::is_test_transport_installed() {
        execute_legacy_enclave(metadosis, ctx, emission_day, calculation)
    } else {
        execute_onchain_fractions(metadosis, ctx, emission_day, calculation)
    }
}
