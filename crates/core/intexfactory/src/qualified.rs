//! Per-block qualification: drains floor-bins crossed by the live COEN rate and
//! qualifies Issued series past their qualification period. Runs in `begin_block`.
//! A floor compares only to its own currency's rate, so each currency walks its own
//! trie with its own cursor; they share one per-block budget.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_intex::SeriesId;
use outbe_oracle::api::{coen_rate_for_opt, get_all_reference_currencies};
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
};

use outbe_intex::IntexState;

use crate::constants::{
    MAX_GROUP_DECISIONS_PER_SWEEP, MAX_SERIES_ACTIONS_PER_SWEEP, MAX_SERIES_PER_MARK,
    ORIGIN_ROUTER_ADDRESS,
};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use crate::state::{Group, UnqualifiedBinTree};

pub struct IntexLifecycle;

impl BlockLifecycle for IntexLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        scan_and_qualify(ctx)?;
        // A call sweep the daily trigger could not finish in one go carries on
        // here, block by block, rather than waiting a day for the next trigger.
        crate::called::run_call_slice(ctx)?;
        // Drain in-flight payouts first, then start rounds for any series whose
        // proceeds fan-in deadline has passed.
        crate::runtime::drain_distributions(&ctx.storage)?;
        crate::runtime::sweep_proceeds_deadlines(&ctx.storage, ctx.block.timestamp)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// Number of series promoted Issued -> Qualified this block. Every reference currency is
/// scanned against its own rate, sharing one [`ScanBudget`]; an unpriced one is skipped
/// for the block rather than halting it.
pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<u32> {
    let currencies = get_all_reference_currencies(ctx)?;
    if currencies.is_empty() {
        return Ok(0);
    }
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let start = factory.qualify_currency_cursor.read()? as usize % currencies.len();

    let mut budget = ScanBudget::new();
    let mut promoted: u32 = 0;
    let mut resume_at = start;
    for offset in 0..currencies.len() {
        let at = (start + offset) % currencies.len();
        if budget.is_spent() {
            // Resume here, so a heavy currency cannot starve the ones behind it.
            resume_at = at;
            break;
        }
        let Some(rate) = coen_rate_for_opt(ctx.storage.clone(), currencies[at])? else {
            continue;
        };
        promoted =
            promoted.saturating_add(qualify_currency(ctx, currencies[at], rate, &mut budget)?);
    }
    factory.qualify_currency_cursor.write(resume_at as u32)?;
    Ok(promoted)
}

/// Qualifies one reference currency's groups, drawing on the shared `budget`.
/// Returns how many series were promoted.
fn qualify_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    rate: U256,
    budget: &mut ScanBudget,
) -> Result<u32> {
    let now = ctx.block.timestamp;
    // Deterministic out-of-range rate: skip this currency instead of halting the block.
    let r_bin = match IntexFactoryContract::price_to_bin(rate) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", iso_code, error = ?e, "qualify scan: rate out of range, skipping currency");
            return Ok(0);
        }
    };
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());
    let qualification_period = crate::config::read(&factory)?.qualification_period;

    let mut promoted: u32 = 0;
    // Cap per-block work and resume next block from a persisted bin cursor: the scan
    // no longer scales with the active-series population. A group qualifies within one full
    // sweep (bounded lag); the resulting state is unchanged.
    let mut cursor: u32 = factory.qualify_scan_cursor.read(&iso_code)?;
    'bins: loop {
        if budget.is_spent() {
            // Between bins, so the next slice resumes at a bin it has not opened.
            factory.qualify_scan_cursor.write(&iso_code, cursor)?;
            break;
        }
        let next = match tree_math::find_first_left_inclusive(
            &UnqualifiedBinTree(&factory, iso_code),
            cursor,
        )? {
            Some(b) if b <= r_bin => b,
            _ => {
                // End of the eligible range: next block starts a fresh sweep from the bottom.
                factory.qualify_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };

        // Snapshot the bin before mutating: a qualified group leaves it.
        for worldwide_day in factory.unqualified_groups_in_bin(iso_code, next)? {
            let group = factory.unqualified_group(iso_code, worldwide_day)?;
            if !budget.admits_actions(group.members.len() as u32) {
                // Stop before the group, leaving the cursor on its bin: the groups
                // already qualified are gone from it, so the next slice resumes here
                // without redoing them.
                factory.qualify_scan_cursor.write(&iso_code, next)?;
                break 'bins;
            }
            budget.spend_decision();
            // Isolate per-group: a deterministic Err rolls back the group's checkpoint and is
            // skipped (logged), so one bad group cannot halt the block. Infra errors that recur
            // every group still surface via the structural reads above, which keep `?`.
            let res = ctx.storage.with_checkpoint(|| {
                try_qualify_group(
                    &ctx.storage,
                    &mut factory,
                    &group,
                    qualification_period,
                    now,
                    rate,
                )
            });
            match res {
                Ok(applied) => {
                    budget.spend_actions(applied);
                    promoted = promoted.saturating_add(applied);
                }
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", iso_code, worldwide_day = %worldwide_day, error = ?e, "qualify scan: skipping group");
                }
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => {
                // Reached the top bin: wrap to a fresh sweep next block.
                factory.qualify_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };
    }
    Ok(promoted)
}

/// Work one scan may do, split by cost: deciding a group is a single read,
/// applying it writes once per series and sends its notice.
pub(crate) struct ScanBudget {
    decisions: u32,
    actions: u32,
}

impl ScanBudget {
    pub(crate) fn new() -> Self {
        Self {
            decisions: MAX_GROUP_DECISIONS_PER_SWEEP,
            actions: MAX_SERIES_ACTIONS_PER_SWEEP,
        }
    }

    pub(crate) fn is_spent(&self) -> bool {
        self.decisions == 0 || self.actions == 0
    }

    /// Groups transition whole, so one is taken on only when all of it fits. A
    /// group wider than the entire allowance would stall the scan forever, so it
    /// runs once the actions are otherwise untouched.
    ///
    /// Only the actions are asked. A transition removes its group from the bin,
    /// so stopping on them resumes past the work already done; decisions leave
    /// the bin as it was, so stopping on them mid-bin would restart on the same
    /// groups for as long as they keep deciding against a move. They are spent
    /// all the same, and bound the scan at the next bin boundary.
    pub(crate) fn admits_actions(&self, members: u32) -> bool {
        members <= self.actions || self.actions == MAX_SERIES_ACTIONS_PER_SWEEP
    }

    pub(crate) fn spend_decision(&mut self) {
        self.decisions = self.decisions.saturating_sub(1);
    }

    pub(crate) fn spend_actions(&mut self, series: u32) {
        self.actions = self.actions.saturating_sub(series);
    }
}

/// Qualify a whole `(reference currency, worldwide day)` group. Every series of
/// one day in one reference currency is issued by the same clearing with the
/// same floor, so one read of the group decides all of them. Returns how many
/// series were promoted.
pub(crate) fn try_qualify_group(
    storage: &StorageHandle<'_>,
    factory: &mut IntexFactoryContract,
    group: &Group,
    qualification_period: u32,
    now: u64,
    rate: U256,
) -> Result<u32> {
    let Some(&first) = group.members.first() else {
        return Ok(0);
    };
    let series = outbe_intex::api::read_series(storage, first)?;
    if series.lifecycle_state()? != IntexState::Issued {
        return Ok(0);
    }
    let qualifies_at = u64::from(series.issued_at).saturating_add(u64::from(qualification_period));
    if now <= qualifies_at {
        return Ok(0);
    }
    if rate <= series.floor_price_minor {
        return Ok(0);
    }

    for &series_id in &group.members {
        outbe_intex::api::mark_qualified(storage, series_id)?;
    }
    factory.remove_unqualified_group(group.iso_code, group.worldwide_day)?;
    factory.insert_qualified_group(
        group.iso_code,
        group.worldwide_day,
        series.call_price_minor,
        &group.members,
    )?;

    // Notify the target chains of the Qualified transition via ERC-7786; best-effort.
    // OriginRouter failure (e.g. exhausted relay float) does not revert the
    // state transition. The target chain can reconcile series state from the origin chain.
    let _ = notify_qualified(storage, group.worldwide_day, &group.members);

    for &series_id in &group.members {
        crate::runtime::emit_event(
            storage,
            crate::precompile::IIntexFactory::SeriesQualified {
                seriesId: series_id.into(),
            },
        )?;
    }
    Ok(group.members.len() as u32)
}

/// One message per group, split only where the wire's cap forces it.
fn notify_qualified(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    members: &[SeriesId],
) -> Result<()> {
    for chunk in members.chunks(MAX_SERIES_PER_MARK) {
        // Relay-float-funded: value 0, so the router self-quotes and pays the bridge fee from its float.
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            IOriginRouter::sendMarkQualifiedCall {
                worldwideDay: worldwide_day.value(),
                seriesIds: chunk.iter().map(|id| (*id).into()).collect(),
            }
            .abi_encode()
            .into(),
        )?;
    }
    Ok(())
}
