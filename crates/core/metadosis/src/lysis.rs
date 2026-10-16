//! Executes one sealed day against the current EVM journal.

use std::collections::{BTreeMap, BTreeSet};

use outbe_ocomp_protocol::intent::DayType;
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
    ocomp_limits::{apply_auction_brief, apply_fresh_request_limit_effect, RequestLimitEffect},
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent, ReadyDisposition},
    schema::MetadosisContract,
    settlement::MetadosisCalculation,
    terminal::model::{LysisCompletionDetail, TerminalReceiptCommon, WwdTerminalReceipt},
};

fn mismatch() -> PrecompileError {
    PrecompileError::fatal("on-chain Lysis input or result mismatch")
}

pub(crate) fn execute(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
    calculation: MetadosisCalculation,
) -> Result<()> {
    let storage = ctx.storage.clone();
    storage.require_metadosis_process_ready()?;
    let day = current.worldwide_day;
    let mut tribute = TributeContract::new(storage.clone());
    let totals = tribute.get_day_totals(day)?;
    if !totals.is_sealed || !totals.initialized || totals.tribute_count == 0 {
        return Err(mismatch());
    }
    let mut tributes = Vec::with_capacity(totals.tribute_count as usize);
    let mut owners = BTreeSet::new();
    let mut currencies = BTreeMap::new();
    let mut fidelity = FidelitySnapshotRequest {
        timestamp: ctx.block.timestamp,
        first_qualified_start: 0,
        entries: Vec::with_capacity(totals.tribute_count as usize),
    };
    let price_day = outbe_nod::openings::entry_price_day(ctx.block.timestamp);
    for record in tribute.state_records_by_day(day)? {
        let encrypted: EncryptedTributeV2 = record.into();
        let source = &encrypted.context;
        if source.chain_id != ctx.block.chain_id
            || !encrypted.has_valid_encoding()
            || !owners.insert(source.owner)
            || outbe_nod::api::get_item(&storage, source.tribute_id)?.is_some()
        {
            return Err(mismatch());
        }
        let section = outbe_fidelity::api::cohort_section(
            storage.clone(),
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
                storage.clone(),
                price_day,
                source.reference_currency,
            )?);
        }
        tributes.push(encrypted);
    }
    if tributes.len() != totals.tribute_count as usize {
        return Err(mismatch());
    }
    let outcome = outbe_tee::lysis::execute(LysisRequest {
        chain_id: ctx.block.chain_id,
        worldwide_day: day,
        timestamp: ctx.block.timestamp,
        lysis_limit_minor: calculation.lysis_limit_minor,
        tributes,
        fidelity,
        entry_prices: currencies.into_iter().collect(),
    })
    .map_err(|_| PrecompileError::EnclaveUnavailable("on-chain Lysis request failed".into()))?;
    let result = match outcome {
        LysisOutcome::Completed(result) => result,
        LysisOutcome::Rejected(_) => {
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
        }
    };
    if result.total_nominal_minor != totals.tribute_nominal_total_minor {
        return Err(mismatch());
    }
    let used = calculation
        .lysis_limit_minor
        .checked_sub(result.unused_lysis_limit_minor)
        .ok_or_else(mismatch)?;
    let profile = crate::runtime::require_active_ocomp_profile(metadosis)?;
    let carry_before = PromisLimitContract::new(storage.clone()).get_total_unallocated()?;
    let split = apply_fresh_request_limit_effect(
        storage.clone(),
        RequestLimitEffect {
            protocol_bundle_hash: profile.protocol_bundle_hash,
            wwd: day.value(),
            pending_nonce: 0,
            day_type: match current.day_type {
                WwdDayType::Green => DayType::Green,
                WwdDayType::Red => DayType::Red,
                WwdDayType::Unknown => return Err(mismatch()),
            },
            day_limit: current.metadosis_limit_minor,
            lysis_limit_minor: calculation.lysis_limit_minor,
            nominal_total: result.total_nominal_minor,
            logical_anchor: ctx.block.timestamp,
        },
    )?;
    if used
        .checked_add(split.desis_limit_minor)
        .is_none_or(|allocation| allocation > result.total_nominal_minor)
    {
        return Err(mismatch());
    }
    for nod in &result.nods {
        outbe_nodfactory::api::issue_lysis_nod(&storage, nod)?;
    }
    let owner_receipt = tribute.consume_and_retire_lysis_partition(
        day,
        totals.tribute_count,
        result.total_nominal_minor,
    )?;
    if owner_receipt.source_generation != 0 || owner_receipt.retired_generation != 1 {
        return Err(mismatch());
    }
    let credit = PromisLimitContract::new(storage.clone())
        .checked_add_carry_over(result.unused_lysis_limit_minor)?;
    if credit.credited != result.unused_lysis_limit_minor {
        return Err(mismatch());
    }
    apply_auction_brief(storage.clone(), &split)?;
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
            retirement: owner_receipt.retirement_outcome,
            block_number: ctx.block.block_number,
        },
        detail: LysisCompletionDetail {
            tribute_count: totals.tribute_count,
            lysis_limit: calculation.lysis_limit_minor,
            used,
            unused: result.unused_lysis_limit_minor,
            desis_limit: split.desis_limit_minor,
            inputs_hash: result.inputs_hash,
            result_hash: result.result_hash,
        },
    })?;
    commit_outer_transition(metadosis, day, &transition, ctx.block.block_number)?;
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: day.into(),
        tributeNominalTotalMinor: result.total_nominal_minor,
        gratisDemandMinor: calculation.gratis_demand,
        dayGratisLimitMinor: calculation.day_gratis_limit_minor,
        lysisLimitMinor: calculation.lysis_limit_minor,
        unusedLysisLimitMinor: result.unused_lysis_limit_minor,
        lysisAllocationMinor: used,
        promisLimitReturnedMinor: result.unused_lysis_limit_minor,
        status: "COMPLETED".into(),
        blockNumber: ctx.block.block_number,
    })
}
