//! Queue of call notices, sent every block after the call slice, and the queue of the legs
//! the router kept for one chain each. A refused notice or a kept leg waits
//! [`NOTICE_RETRY_SECONDS`] before its next try, and gets at most [`MAX_CALL_NOTICE_ATTEMPTS`].

use alloy_primitives::{Bytes, FixedBytes, U256};
use alloy_sol_types::{SolCall, SolError, SolValue};
use outbe_intex::SeriesId;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_system_common::pass_budget::PassBudget;
use outbe_system_common::time::EmissionDate;

use crate::called::revert_data;
use crate::constants::{
    MAX_CALL_NOTICE_ATTEMPTS, MAX_REFUSED_RUNS_PER_BLOCK, MAX_SERIES_PER_MARK,
    NOTICE_RETRY_SECONDS, ORIGIN_ROUTER_ADDRESS,
};
use crate::precompile::IIntexFactory::IntexSeriesCallNoticeSendFailed;
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;

/// Bit offset of the refused-attempt count, in the byte above the call time.
const ATTEMPTS_SHIFT: usize = 64;
/// Bit offset of the time a refused entry may go out again, above the attempt count.
const RETRY_AT_SHIFT: usize = 72;
const _: () = assert!(RETRY_AT_SHIFT + 64 <= 256 - 8 * outbe_intex::SERIES_ID_LEN);

/// A Called entry packs its call time into the low bytes the 14-byte `SeriesId` leaves
/// free, so the origin's stamp reaches the target instead of its delivery time.
pub fn pack_call_notice(series_id: SeriesId, called_at: u64) -> U256 {
    series_id.to_word() | U256::from(called_at)
}

/// Router calls a queued Called entry has been refused so far.
pub fn call_notice_attempts(entry: U256) -> u8 {
    ((entry >> ATTEMPTS_SHIFT) & U256::from(u8::MAX)).to::<u8>()
}

fn with_attempts(entry: U256, attempts: u8) -> U256 {
    (entry & !(U256::from(u8::MAX) << ATTEMPTS_SHIFT)) | (U256::from(attempts) << ATTEMPTS_SHIFT)
}

/// When a queued entry may be sent. 0 for a fresh one.
pub fn notice_retry_at(entry: U256) -> u64 {
    ((entry >> RETRY_AT_SHIFT) & U256::from(u64::MAX)).to::<u64>()
}

pub fn with_retry_at(entry: U256, retry_at: u64) -> U256 {
    (entry & !(U256::from(u64::MAX) << RETRY_AT_SHIFT)) | (U256::from(retry_at) << RETRY_AT_SHIFT)
}

/// Bit offsets of a kept leg: the router's message index in the low 64 bits, then the chain,
/// the failed tries and the time it may go out again.
const LEG_CHAIN_SHIFT: usize = 64;
const LEG_ATTEMPTS_SHIFT: usize = 96;
const LEG_RETRY_AT_SHIFT: usize = 104;

/// A leg the router kept for one chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptLeg {
    /// The router's index of the kept message.
    pub idx: u64,
    pub dst_chain_id: u32,
    /// Failed tries so far, the first send included.
    pub attempts: u8,
    pub retry_at: u64,
}

impl KeptLeg {
    pub fn pack(self) -> U256 {
        U256::from(self.idx)
            | (U256::from(self.dst_chain_id) << LEG_CHAIN_SHIFT)
            | (U256::from(self.attempts) << LEG_ATTEMPTS_SHIFT)
            | (U256::from(self.retry_at) << LEG_RETRY_AT_SHIFT)
    }

    pub fn unpack(word: U256) -> Self {
        Self {
            idx: (word & U256::from(u64::MAX)).to::<u64>(),
            dst_chain_id: ((word >> LEG_CHAIN_SHIFT) & U256::from(u32::MAX)).to::<u32>(),
            attempts: ((word >> LEG_ATTEMPTS_SHIFT) & U256::from(u8::MAX)).to::<u8>(),
            retry_at: ((word >> LEG_RETRY_AT_SHIFT) & U256::from(u64::MAX)).to::<u64>(),
        }
    }
}

/// Whether a Called entry belongs to the run a message is being built for. The wire carries
/// one day and one call time for the whole batch, so both must match for a series to ride along.
pub fn joins_run(day: EmissionDate, called_at: u64, series_day: EmissionDate, ts: u64) -> bool {
    ts == called_at && series_day == day
}

/// The Emission Day the series' auction ran on; `None` once the series is deleted.
fn series_day(storage: &StorageHandle<'_>, id: SeriesId) -> Result<Option<EmissionDate>> {
    Ok(outbe_intex::api::find_series(storage, id)?.map(|series| series.emission_day))
}

fn unpack_call_notice(entry: U256) -> (SeriesId, u64) {
    (
        SeriesId::from_word(entry),
        (entry & U256::from(u64::MAX)).to::<u64>(),
    )
}

pub(crate) fn enqueue_notice(factory: &IntexFactoryContract, entry: U256) -> Result<()> {
    let tail = factory.notify_tail.read()?;
    factory.notify_at.write(&tail, entry)?;
    factory.notify_tail.write(tail.saturating_add(1))?;
    Ok(())
}

pub(crate) fn enqueue_leg(factory: &IntexFactoryContract, leg: KeptLeg) -> Result<()> {
    let tail = factory.leg_tail.read()?;
    factory.leg_at.write(&tail, leg.pack())?;
    factory.leg_tail.write(tail.saturating_add(1))?;
    Ok(())
}

/// Sends the queued notices, one write of the sweep budget per router call.
/// The walk stops at the first entry not yet due: a refused one waits its pause, and the
/// queue behind it waits no longer than one pause.
fn run_notice_unit(
    ctx: &BlockRuntimeContext,
    send: &mut NoticeSend<'_, '_>,
    index: u32,
) -> Result<Option<(u32, bool)>> {
    match ctx.storage.with_checkpoint(|| send.run(index)) {
        Ok(done) => Ok(Some(done)),
        Err(error) if error.pass_failure() == outbe_primitives::error::PassFailure::Stop => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn send_call_notices(ctx: &BlockRuntimeContext) -> Result<()> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let mut send = NoticeSend {
        factory: &factory,
        storage: &ctx.storage,
        now: ctx.block.timestamp,
        stop: factory.notify_tail.read()?,
        budget: PassBudget::per_block(),
    };
    let refused_runs = send_notices(ctx, &mut send)?;
    retry_kept_legs(&mut send, refused_runs)
}

/// Walks the notice queue. Returns the count of wholly refused runs at its end.
fn send_notices(ctx: &BlockRuntimeContext, send: &mut NoticeSend<'_, '_>) -> Result<u32> {
    let factory = send.factory;
    let head = factory.notify_head.read()?;
    let tail = send.stop;
    if head >= tail {
        return Ok(0);
    }
    let mut index = head;
    let mut refused_runs: u32 = 0;
    while index < tail
        && !send.budget.spent()
        && refused_runs < MAX_REFUSED_RUNS_PER_BLOCK
        && notice_retry_at(factory.notify_at.read(&index)?) <= send.now
    {
        let Some((consumed, refused)) = run_notice_unit(ctx, send, index)? else {
            break;
        };
        index += consumed;
        refused_runs = if refused { refused_runs + 1 } else { 0 };
    }
    if index >= factory.notify_tail.read()? {
        factory.notify_head.write(0)?;
        factory.notify_tail.write(0)?;
    } else {
        factory.notify_head.write(index)?;
    }
    Ok(refused_runs)
}

/// Sends the due kept legs again, one write of the budget per leg. The walk stops at the
/// first leg not yet due, and at a lack of gas, which counts no try.
fn retry_kept_legs(send: &mut NoticeSend<'_, '_>, mut refused_runs: u32) -> Result<()> {
    let factory = send.factory;
    let head = factory.leg_head.read()?;
    let tail = factory.leg_tail.read()?;
    if head >= tail {
        return Ok(());
    }
    let mut index = head;
    while index < tail && !send.budget.spent() && refused_runs < MAX_REFUSED_RUNS_PER_BLOCK {
        let leg = KeptLeg::unpack(factory.leg_at.read(&index)?);
        if leg.retry_at > send.now {
            break;
        }
        send.budget.admit_writes(1);
        let Some(failed) = send.retry_leg(leg)? else {
            break;
        };
        factory.leg_at.clear(&index)?;
        index += 1;
        refused_runs = if failed { refused_runs + 1 } else { 0 };
    }
    if index >= factory.leg_tail.read()? {
        factory.leg_head.write(0)?;
        factory.leg_tail.write(0)?;
    } else {
        factory.leg_head.write(index)?;
    }
    Ok(())
}

/// One block's send: the queue window it walks and the router calls left.
struct NoticeSend<'a, 'storage> {
    factory: &'a IntexFactoryContract<'storage>,
    storage: &'a StorageHandle<'storage>,
    now: u64,
    /// End of this block's window. Entries requeued behind it wait for a later block.
    stop: u32,
    budget: PassBudget,
}

impl NoticeSend<'_, '_> {
    /// Send the run of Called entries starting at `at` that shares its day and call time.
    /// `notify_called` splits the run where the wire's cap forces it. Returns the entries
    /// consumed and whether the router refused all of them.
    fn run(&mut self, at: u32) -> Result<(u32, bool)> {
        let factory = self.factory;
        let first = factory.notify_at.read(&at)?;
        let calls_left = self.budget.writes_left();
        let (first_id, called_at) = unpack_call_notice(first);
        // A series is deleted only when nothing of it is left to call.
        let Some(emission_day) = series_day(self.storage, first_id)? else {
            factory.notify_at.clear(&at)?;
            return Ok((1, false));
        };
        let mut run = vec![first_id];
        let mut entries = vec![first];

        // The run is what one message carries, so it is cut to the calls still budgeted
        // rather than to the whole block's window.
        let run_cap = (calls_left as usize).saturating_mul(MAX_SERIES_PER_MARK);
        let mut index = at.saturating_add(1);
        while index < self.stop && run.len() < run_cap {
            let entry = factory.notify_at.read(&index)?;
            let (id, ts) = unpack_call_notice(entry);
            if notice_retry_at(entry) > self.now
                || !series_day(self.storage, id)?
                    .is_some_and(|day| joins_run(emission_day, called_at, day, ts))
            {
                break;
            }
            run.push(id);
            entries.push(entry);
            index += 1;
        }

        for slot in at..index {
            factory.notify_at.clear(&slot)?;
        }
        self.budget.admit_writes(router_calls(run.len()));
        let outcome = crate::called::notify_called(self.storage, emission_day, called_at, &run)?;
        let all_refused = outcome.refused.len() == run.len();
        // A kept leg failed one try more than its run had.
        let prior = entries
            .iter()
            .map(|e| call_notice_attempts(*e))
            .max()
            .unwrap_or(0);
        for leg in &outcome.kept {
            enqueue_leg(
                self.factory,
                KeptLeg {
                    idx: leg.idx.saturating_to::<u64>(),
                    dst_chain_id: leg.dstChainId,
                    attempts: prior.saturating_add(1),
                    retry_at: self.now.saturating_add(NOTICE_RETRY_SECONDS),
                },
            )?;
        }
        // A refused entry goes behind this block's window and waits out its pause.
        for entry in entries {
            let series_id = unpack_call_notice(entry).0;
            if let Some((_, error_data)) = outcome.refused.iter().find(|(id, _)| *id == series_id) {
                self.requeue_refused(entry, emission_day, error_data.clone())?;
            }
        }
        Ok((index - at, all_refused))
    }

    fn requeue_refused(
        &self,
        entry: U256,
        emission_day: EmissionDate,
        error_data: Bytes,
    ) -> Result<()> {
        let attempts = call_notice_attempts(entry).saturating_add(1);
        if attempts < MAX_CALL_NOTICE_ATTEMPTS {
            let retry_at = self.now.saturating_add(NOTICE_RETRY_SECONDS);
            return enqueue_notice(
                self.factory,
                with_retry_at(with_attempts(entry, attempts), retry_at),
            );
        }
        let (series_id, called_at) = unpack_call_notice(entry);
        tracing::warn!(
            target: "outbe::intexfactory",
            series = %series_id,
            called_at,
            attempts,
            "call notice: all sends failed"
        );
        // The router refused the whole message, so no chain of the day took it. Chain 0 stands
        // for the day when the router cannot name its chains.
        let chains = match self.targets_of(emission_day) {
            Ok(chains) if !chains.is_empty() => chains,
            Err(error) if error.pass_failure() == PassFailure::Propagate => return Err(error),
            _ => vec![0],
        };
        for dst_chain_id in chains {
            self.emit_send_failed(series_id.into(), dst_chain_id, error_data.clone())?;
        }
        Ok(())
    }

    /// Sends one kept leg again. Returns whether the try failed, or `None` when gas ran out.
    fn retry_leg(&mut self, leg: KeptLeg) -> Result<Option<bool>> {
        let sent = self.storage.with_checkpoint(|| {
            self.storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::retryMessageSendCall {
                    idx: U256::from(leg.idx),
                }
                .abi_encode()
                .into(),
            )
        });
        let error = match sent {
            Ok(_) => return Ok(Some(false)),
            Err(error) => error,
        };
        match error.pass_failure() {
            PassFailure::Propagate => return Err(error),
            PassFailure::Stop => return Ok(None),
            PassFailure::Skip => {}
        }
        let error_data = revert_data(error);
        // Another account sent the kept message first.
        if error_data.starts_with(&IOriginRouter::NoFailedMessageSend::SELECTOR) {
            return Ok(Some(false));
        }
        let attempts = leg.attempts.saturating_add(1);
        if attempts < MAX_CALL_NOTICE_ATTEMPTS {
            enqueue_leg(
                self.factory,
                KeptLeg {
                    attempts,
                    retry_at: self.now.saturating_add(NOTICE_RETRY_SECONDS),
                    ..leg
                },
            )?;
            return Ok(Some(true));
        }
        tracing::warn!(
            target: "outbe::intexfactory",
            idx = leg.idx,
            dst_chain_id = leg.dst_chain_id,
            attempts,
            "call notice: all tries to a chain failed, the message stays kept"
        );
        let series = match self.kept_series(leg.idx) {
            Ok(series) => series,
            Err(error) if error.pass_failure() == PassFailure::Propagate => return Err(error),
            Err(_) => Vec::new(),
        };
        for series_id in series {
            self.emit_send_failed(series_id, leg.dst_chain_id, error_data.clone())?;
        }
        Ok(Some(true))
    }

    /// The series of a kept call notice: its body is `abi.encode(emissionDay, calledAt, seriesIds)`
    /// after the two header bytes.
    fn kept_series(&self, idx: u64) -> Result<Vec<FixedBytes<14>>> {
        let returned = self.storage.staticcall(
            ORIGIN_ROUTER_ADDRESS,
            IOriginRouter::getFailedMessageSendCall {
                idx: U256::from(idx),
            }
            .abi_encode()
            .into(),
        )?;
        let kept = IOriginRouter::getFailedMessageSendCall::abi_decode_returns(&returned)
            .map_err(|_| PrecompileError::RevertBytes(returned.clone()))?;
        let body = kept.payload.get(2..).unwrap_or_default();
        let (_, _, series) = <(u32, u64, Vec<FixedBytes<14>>)>::abi_decode_params(body)
            .map_err(|_| PrecompileError::RevertBytes(kept.payload.clone()))?;
        Ok(series)
    }

    fn targets_of(&self, emission_day: EmissionDate) -> Result<Vec<u32>> {
        let returned = self.storage.staticcall(
            ORIGIN_ROUTER_ADDRESS,
            IOriginRouter::targetsOfCall {
                emissionDay: emission_day.value(),
            }
            .abi_encode()
            .into(),
        )?;
        IOriginRouter::targetsOfCall::abi_decode_returns(&returned)
            .map_err(|_| PrecompileError::RevertBytes(returned))
    }

    fn emit_send_failed(
        &self,
        series_id: FixedBytes<14>,
        dst_chain_id: u32,
        error_data: Bytes,
    ) -> Result<()> {
        crate::runtime::emit_event(
            self.storage,
            IntexSeriesCallNoticeSendFailed {
                seriesId: series_id,
                dstChainId: dst_chain_id,
                errorData: error_data,
            },
        )
    }
}

/// Router calls a batch of this many series costs: the wire caps a mark at
/// [`MAX_SERIES_PER_MARK`], and each call fans out to the day's target chains.
fn router_calls(series: usize) -> u32 {
    series.div_ceil(MAX_SERIES_PER_MARK) as u32
}
