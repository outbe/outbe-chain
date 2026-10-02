//! Execution attributes and system-zone plans for one payload attempt.

use alloy_primitives::Bytes;
use outbe_evm::{AccountedParentArtifact, OutbeEvmConfig, OutbeNextBlockEnvAttributes};
use outbe_primitives::{
    reshare_artifact::{decode_outbe_block_artifacts, sanitize_prefinal_outbe_block_artifacts},
    OutbeHeader, OutbePayloadAttributes, OutbeTxEnvelope,
};
use reth_chainspec::{ChainSpec, EthChainSpec};
use reth_evm::{execute::BlockBuilder, NextBlockEnvAttributes};
use reth_payload_primitives::PayloadBuilderError;
use reth_primitives_traits::{AlloyBlockHeader as _, Recovered, SealedHeader};
use tracing::{debug, warn};

use super::ce_local_readiness_error;

/// Immutable identity and attributes shared by the preparation stages.
pub(super) struct PayloadContext<'a> {
    pub(super) parent: &'a SealedHeader<OutbeHeader>,
    pub(super) attributes: &'a OutbePayloadAttributes,
    pub(super) chain_spec: &'a ChainSpec<OutbeHeader>,
}

/// Execution context and body witnesses share the same signed Phase-1 bytes.
pub(super) struct PreparedPayload {
    pub(super) env: OutbeNextBlockEnvAttributes,
    pub(super) system_inputs: SystemTransactionInputs,
}

pub(super) struct SystemTransactionInputs {
    prefinal_extra_data: Bytes,
    prebuilt_phase1_tx: Option<Recovered<OutbeTxEnvelope>>,
    pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
}

pub(super) fn prepare(
    evm_config: &OutbeEvmConfig,
    context: &PayloadContext<'_>,
) -> Result<PreparedPayload, PayloadBuilderError> {
    let PayloadContext {
        parent: parent_header,
        attributes,
        chain_spec,
    } = context;
    let inner = attributes.inner();
    let block_number = parent_header.number().saturating_add(1);
    let prefinal_extra_data = sanitize_prefinal_outbe_block_artifacts(attributes.extra_data())
        .map_err(PayloadBuilderError::other)?;

    // / / prebuild and sign the Phase 1
    // (CertifiedParentAccounting) body[0] tx BEFORE the executor enters
    // `apply_pre_execution_changes`. The same `Recovered` is then handed
    // to `build_begin_system_txs` for body[0] so the pre-exec commit
    // witness and the body[0] tx are byte-identical (hash match). For
    // `block_number <= OutbeProtocolSchedule.genesis_bootstrap_block_number`
    // (greenfield: block 0 / block 1) the helper returns `None` and
    // Phase 1 is skipped entirely.
    let prebuilt_phase1_tx = evm_config
        .build_signed_phase1_tx(
            block_number,
            chain_spec.chain().id(),
            parent_header.hash(),
            attributes.parent_consensus_metadata().cloned(),
            attributes.proposer_evm_address(),
        )
        .map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to prebuild Phase 1 system tx");
            PayloadBuilderError::Internal(err.into())
        })?;

    // decode the parent's accounted-parent artifact from
    // `parent_header.extra_data` so the executor has a fallback when the
    // [`AccountedParentArtifactProvider`] cannot see the parent (e.g.,
    // unfinalized side-chain whose header hasn't been indexed yet). The
    // executor validates this hint against the Phase 1 metadata's
    // `(finalized_block_number, finalized_block_hash)` before accepting it.
    let parent_artifact_hint = decode_outbe_block_artifacts(parent_header.extra_data().as_ref())
        .ok()
        .and_then(|artifacts| artifacts.execution_summary)
        .map(|summary| AccountedParentArtifact {
            summary,
            timestamp: parent_header.timestamp(),
            state_root: Some(parent_header.state_root()),
        });

    // One-time TEE bootstrap: the consensus thread's TEE DKG coordination
    // stashes the assembled `TeeBootstrapV2` in the bridge; the proposer
    // clones it here and injects it into the begin-zone (slice 5.1). Only
    // the proposer's bridge is read; validators verify the body-carried
    // payload (slice 5.2). Candidate construction is retryable, so a rejected
    // first candidate must not consume the only copy needed by later views.
    //
    // Guard to block 1 (the fixed `committee_snapshot_block` target): every
    // node stashes its pending payload at startup, but only the block-1
    // proposer must inject it. Without this guard a node that did not propose
    // block 1 would still hold its pending payload and inject a stale
    // `TeeBootstrap` when it later proposes block N > 1 - which the executor
    // rejects (`committee_snapshot_block` mismatch / already bootstrapped),
    // stalling that slot.
    let pending_tee_bootstrap = if block_number == 1 {
        evm_config
            .bridge
            .as_ref()
            .and_then(|bridge| bridge.pending_tee_bootstrap())
    } else {
        None
    };

    let env = OutbeNextBlockEnvAttributes {
        inner: NextBlockEnvAttributes {
            timestamp: inner.timestamp,
            suggested_fee_recipient: inner.suggested_fee_recipient,
            prev_randao: inner.prev_randao,
            gas_limit: outbe_primitives::system_tx::protocol_block_gas_limit(block_number),
            parent_beacon_block_root: inner.parent_beacon_block_root,
            withdrawals: inner.withdrawals.clone().map(Into::into),
            extra_data: prefinal_extra_data.clone(),
            slot_number: inner.slot_number,
        },
        timestamp_millis_part: attributes.timestamp_millis_part(),
        parent_consensus_metadata: attributes.parent_consensus_metadata().cloned(),
        proposer_evm_address: attributes.proposer_evm_address(),
        execute_outbe_block_hooks: true,
        prebuilt_phase1_tx: prebuilt_phase1_tx.clone(),
        parent_artifact_hint,
        // Clone: the executor branch (expected begin-zone order /
        // `block_has_tee_bootstrap`) needs the same payload the body
        // builder injects below, so both deterministic paths agree.
        pending_tee_bootstrap: pending_tee_bootstrap.clone(),
        execution_read_budget: attributes.execution_read_budget().cloned(),
    };
    Ok(PreparedPayload {
        env,
        system_inputs: SystemTransactionInputs {
            prefinal_extra_data,
            prebuilt_phase1_tx,
            pending_tee_bootstrap,
        },
    })
}

pub(super) fn apply_pre_execution_changes(
    builder: &mut impl BlockBuilder,
) -> Result<(), PayloadBuilderError> {
    if let Err(err) = builder.apply_pre_execution_changes() {
        if ce_local_readiness_error(&err) {
            // This attempt raced finalization and cannot use the in-place
            // materialization for its old parent. Report a retryable build
            // failure: Reth reserves `BuildOutcome::Cancelled` for futures
            // whose supplied cancel signal actually fired and treats any
            // other use as an unreachable invariant violation.
            debug!(target: "payload_builder", %err, "payload exact-parent data is no longer locally available; retrying on the next build tick");
            return Err(PayloadBuilderError::Internal(err.into()));
        }
        warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
        return Err(PayloadBuilderError::Internal(err.into()));
    }

    Ok(())
}

/// Both zones are planned before any begin transaction executes so the user
/// selection budget can reserve the complete terminal zone.
pub(super) struct SystemTransactions {
    pub(super) begin: Vec<Recovered<OutbeTxEnvelope>>,
    pub(super) end: Vec<Recovered<OutbeTxEnvelope>>,
}

impl SystemTransactions {
    pub(super) fn build(
        evm_config: &OutbeEvmConfig,
        context: &PayloadContext<'_>,
        block_gas_limit: u64,
        inputs: SystemTransactionInputs,
    ) -> Result<Self, PayloadBuilderError> {
        let PayloadContext {
            parent: parent_header,
            attributes,
            chain_spec,
        } = context;
        let block_number = parent_header.number().saturating_add(1);
        let begin_system_txs = evm_config
            .build_begin_system_txs(
                block_number,
                chain_spec.chain().id(),
                block_gas_limit,
                parent_header.hash(),
                &inputs.prefinal_extra_data,
                attributes.parent_consensus_metadata().cloned(),
                attributes.proposer_evm_address(),
                // reuse the prebuilt body[0] tx
                // byte-for-byte. `build_begin_system_txs` validates
                // calldata + signer match before substitution.
                inputs.prebuilt_phase1_tx,
                // The same bootstrap payload the executor branch above received,
                // so the injected body matches the expected begin-zone order
                // (TeeBootstrap at begin_order 3, before OracleSlashWindow).
                inputs.pending_tee_bootstrap,
            )
            .map_err(|err| {
                warn!(target: "payload_builder", %err, "failed to build begin system transactions");
                PayloadBuilderError::Internal(err.into())
            })?;
        let begin_system_tx_count = begin_system_txs.len();
        let end_system_txs = evm_config
            .build_end_system_txs(
                block_number,
                chain_spec.chain().id(),
                begin_system_tx_count,
                attributes.proposer_evm_address(),
            )
            .map_err(|err| {
                warn!(target: "payload_builder", %err, "failed to build end system transactions");
                PayloadBuilderError::Internal(err.into())
            })?;
        Ok(Self {
            begin: begin_system_txs,
            end: end_system_txs,
        })
    }
}
