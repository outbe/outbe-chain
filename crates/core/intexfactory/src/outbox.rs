//! The outbox of IntexFactory and its `flush()`, which the begin-block step calls after the Cycle.

use alloy_primitives::{Bytes, FixedBytes, LogData, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use outbe_intex::outbox::{Label, Message, Outcome};
use outbe_intex::wire::{MSG_INTEX_SERIES, MSG_INTEX_SERIES_CALL_NOTICE};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PassFailure, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::INTEX_FACTORY_ADDRESS;
use outbe_system_common::pass_budget::SWEEP_WRITES_PER_BLOCK;

use crate::precompile::IIntexFactory;
use crate::schema::IntexFactoryContract;

/// Puts one message for each of `chains` into the outbox.
pub(crate) fn push_to(
    storage: &StorageHandle<'_>,
    chains: &[u32],
    label: Label,
    message_gas: u64,
    payload: &Bytes,
) -> Result<()> {
    let outbox = IntexFactoryContract::new(storage.clone()).outbox();
    for &chain_id in chains {
        outbox.push(Message {
            chain_id,
            label,
            message_gas,
            payload: payload.clone(),
        })?;
    }
    Ok(())
}

pub(crate) fn flush(storage: &StorageHandle<'_>) -> Result<()> {
    let now = storage.timestamp()?.saturating_to::<u64>();
    let chains = crate::targets::series_target_chains(storage)?;
    IntexFactoryContract::new(storage.clone()).outbox().flush(
        storage,
        &chains,
        now,
        SWEEP_WRITES_PER_BLOCK,
        |outcome| {
            for log in logs(outcome) {
                storage.emit_event(INTEX_FACTORY_ADDRESS, log)?;
            }
            Ok(())
        },
    )
}

/// The series ids that a call notice carries: `abi.encode(emissionDay, calledAt, seriesIds)`
/// after the two header bytes.
fn call_notice_series(payload: &Bytes) -> Vec<FixedBytes<14>> {
    payload
        .get(2..)
        .and_then(|body| <(u32, u64, Vec<FixedBytes<14>>)>::abi_decode_params(body).ok())
        .map(|(_, _, ids)| ids)
        .unwrap_or_default()
}

fn logs(outcome: Outcome) -> Vec<LogData> {
    match outcome {
        Outcome::Sent {
            chain_id,
            label,
            message_id,
            payload,
        } => sent_logs(label, chain_id, message_id, &payload),
        Outcome::Failed {
            chain_id,
            label,
            failures,
            payload,
            error_data,
        } => failed_logs(label, chain_id, failures, &payload, error_data),
    }
}

fn sent_logs(label: Label, chain_id: u32, send_id: B256, payload: &Bytes) -> Vec<LogData> {
    match label.msg_type {
        MSG_INTEX_SERIES => vec![IIntexFactory::IntexSeriesSent {
            sendId: send_id,
            emissionDay: label.emission_day,
            dstChainId: chain_id,
            pageIndex: label.fields[0],
            totalPages: label.fields[1],
            seriesCount: label.fields[2],
        }
        .encode_log_data()],
        MSG_INTEX_SERIES_CALL_NOTICE => call_notice_series(payload)
            .into_iter()
            .map(|series_id| {
                IIntexFactory::IntexSeriesCallNoticeSent {
                    sendId: send_id,
                    seriesId: series_id,
                    dstChainId: chain_id,
                }
                .encode_log_data()
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn failed_logs(
    label: Label,
    chain_id: u32,
    failures: u32,
    payload: &Bytes,
    error_data: Bytes,
) -> Vec<LogData> {
    match label.msg_type {
        MSG_INTEX_SERIES => vec![IIntexFactory::IntexSeriesSendFailed {
            emissionDay: label.emission_day,
            dstChainId: chain_id,
            pageIndex: label.fields[0],
            failures,
            errorData: error_data,
        }
        .encode_log_data()],
        MSG_INTEX_SERIES_CALL_NOTICE => call_notice_series(payload)
            .into_iter()
            .map(|series_id| {
                IIntexFactory::IntexSeriesCallNoticeSendFailed {
                    seriesId: series_id,
                    dstChainId: chain_id,
                    failures,
                    errorData: error_data.clone(),
                }
                .encode_log_data()
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn outbox_head(
    storage: &StorageHandle<'_>,
    chain_id: u32,
) -> Result<IIntexFactory::outboxHeadReturn> {
    let outbox = IntexFactoryContract::new(storage.clone()).outbox();
    let pending = outbox.len(chain_id)?;
    let head = outbox.head_of(chain_id)?;
    Ok(IIntexFactory::outboxHeadReturn {
        msgType: head.map_or(0, |h| h.label.msg_type),
        emissionDay: head.map_or(0, |h| h.label.emission_day),
        pending,
        failures: head.map_or(0, |h| h.failures),
        nextTry: head.map_or(0, |h| h.next_try),
    })
}

/// Begin-block step after the Cycle: sub-calls `flush()`, so IntexFactory is the caller of
/// CrosschainBridge. A failed flush leaves the messages for the next block.
pub struct IntexFactoryOutboxModule;

impl outbe_primitives::block::ProtocolModule for IntexFactoryOutboxModule {
    const NAME: &'static str = "intexfactory_outbox";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        let call = IIntexFactory::flushCall {}.abi_encode().into();
        match ctx.storage.call(INTEX_FACTORY_ADDRESS, U256::ZERO, call) {
            Err(error) if error.pass_failure() == PassFailure::Propagate => Err(error),
            Err(error) => {
                tracing::warn!(target: "outbe::intexfactory", %error, "outbox flush skipped");
                Ok(())
            }
            Ok(_) => Ok(()),
        }
    }
}
