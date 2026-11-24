//! The outbox of Desis and its `flush()`, which the begin-block step calls after the Cycle.

use alloy_primitives::U256;
use alloy_sol_types::{SolCall, SolEvent};
use outbe_intex::outbox::{Label, Outcome};
use outbe_intex::wire::{
    MSG_INTEX_AUCTION_CANCEL, MSG_INTEX_AUCTION_CLEARING_START, MSG_INTEX_AUCTION_RESULT,
    MSG_INTEX_AUCTION_START, MSG_INTEX_AUCTION_WINNERS,
};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PassFailure, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::DESIS_ADDRESS;
use outbe_system_common::pass_budget::SWEEP_WRITES_PER_BLOCK;

use crate::precompile::IDesis;
use crate::schema::DesisContract;

pub(crate) fn flush(storage: &StorageHandle<'_>) -> Result<()> {
    let now = storage.timestamp()?.saturating_to::<u64>();
    let chains = outbe_intexfactory::targets::series_target_chains(storage)?;
    DesisContract::new(storage.clone()).outbox().flush(
        storage,
        &chains,
        now,
        SWEEP_WRITES_PER_BLOCK,
        |outcome| emit(storage, outcome),
    )
}

fn emit(storage: &StorageHandle<'_>, outcome: Outcome) -> Result<()> {
    let log = match outcome {
        Outcome::Sent {
            chain_id,
            label,
            message_id,
            ..
        } => sent_log(label, chain_id, message_id),
        Outcome::Failed {
            chain_id,
            label,
            failures,
            error_data,
            ..
        } => failed_log(label, chain_id, failures, error_data),
    };
    match log {
        Some(log) => storage.emit_event(DESIS_ADDRESS, log),
        None => Ok(()),
    }
}

fn sent_log(
    label: Label,
    chain_id: u32,
    send_id: alloy_primitives::B256,
) -> Option<alloy_primitives::LogData> {
    let day = label.emission_day;
    let [a, b, c] = label.fields;
    Some(match label.msg_type {
        MSG_INTEX_AUCTION_START => IDesis::IntexAuctionStartSent {
            sendId: send_id,
            emissionDay: day,
            dstChainId: chain_id,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_CANCEL => IDesis::IntexAuctionCancelSent {
            sendId: send_id,
            emissionDay: day,
            dstChainId: chain_id,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_CLEARING_START => IDesis::IntexAuctionClearingStartSent {
            sendId: send_id,
            emissionDay: day,
            dstChainId: chain_id,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_RESULT => IDesis::IntexAuctionResultSent {
            sendId: send_id,
            emissionDay: day,
            dstChainId: chain_id,
            wonQuantity: a,
            wonBidsCount: b,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_WINNERS => IDesis::IntexAuctionWinnersSent {
            sendId: send_id,
            emissionDay: day,
            dstChainId: chain_id,
            pageIndex: a,
            totalPages: b,
            winnersCount: c,
        }
        .encode_log_data(),
        _ => return None,
    })
}

fn failed_log(
    label: Label,
    chain_id: u32,
    failures: u32,
    error_data: alloy_primitives::Bytes,
) -> Option<alloy_primitives::LogData> {
    let day = label.emission_day;
    Some(match label.msg_type {
        MSG_INTEX_AUCTION_START => IDesis::IntexAuctionStartSendFailed {
            emissionDay: day,
            dstChainId: chain_id,
            failures,
            errorData: error_data,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_CANCEL => IDesis::IntexAuctionCancelSendFailed {
            emissionDay: day,
            dstChainId: chain_id,
            failures,
            errorData: error_data,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_CLEARING_START => IDesis::IntexAuctionClearingStartSendFailed {
            emissionDay: day,
            dstChainId: chain_id,
            failures,
            errorData: error_data,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_RESULT => IDesis::IntexAuctionResultSendFailed {
            emissionDay: day,
            dstChainId: chain_id,
            failures,
            errorData: error_data,
        }
        .encode_log_data(),
        MSG_INTEX_AUCTION_WINNERS => IDesis::IntexAuctionWinnersSendFailed {
            emissionDay: day,
            dstChainId: chain_id,
            pageIndex: label.fields[0],
            failures,
            errorData: error_data,
        }
        .encode_log_data(),
        _ => return None,
    })
}

pub(crate) fn outbox_head(
    storage: &StorageHandle<'_>,
    chain_id: u32,
) -> Result<IDesis::outboxHeadReturn> {
    let outbox = DesisContract::new(storage.clone()).outbox();
    let pending = outbox.len(chain_id)?;
    let head = outbox.head_of(chain_id)?;
    Ok(IDesis::outboxHeadReturn {
        msgType: head.map_or(0, |h| h.label.msg_type),
        emissionDay: head.map_or(0, |h| h.label.emission_day),
        pending,
        failures: head.map_or(0, |h| h.failures),
        nextTry: head.map_or(0, |h| h.next_try),
    })
}

/// Begin-block step after the Cycle: sub-calls `flush()`, so Desis is the caller of
/// CrosschainBridge. A failed flush leaves the messages for the next block.
pub struct DesisOutboxModule;

impl outbe_primitives::block::ProtocolModule for DesisOutboxModule {
    const NAME: &'static str = "desis_outbox";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        let call = IDesis::flushCall {}.abi_encode().into();
        match ctx.storage.call(DESIS_ADDRESS, U256::ZERO, call) {
            Err(error) if error.pass_failure() == PassFailure::Propagate => Err(error),
            Err(error) => {
                tracing::warn!(target: "outbe::desis", %error, "outbox flush skipped");
                Ok(())
            }
            Ok(_) => Ok(()),
        }
    }
}
