//! Reconcile finality before any replay, nonce rebuild, or process-identity discard.
use super::{rpc_port::*, tx_journal::*};
use crate::{in_process::InProcessTeeContext, registry_view::NodeBindingSelectorV1};
use alloy_primitives::Address;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceTxReconciliationV1 {
    Empty,
    Completed,
    IngestOnboarding,
    DiscardLostIdentity {
        mutation_finalized: bool,
    },
    ReplayExact,
    ReplaceLostIdentity {
        nonce: u64,
        proof: LostIdentityProof,
    },
    WaitForFinality {
        latest_nonce: u64,
        pending_nonce: u64,
    },
    RebuildAndResign {
        nonce: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Port(#[from] LifecyclePortError<E>),
    #[error(transparent)]
    Journal(#[from] ServiceTxJournalError),
    #[error("live identity codec rejected")]
    Identity(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("finalized checkpoint, chain identity or schedule disagrees")]
    CheckpointMismatch,
    #[error("finalized transaction receipt disagrees with canonical service mutation")]
    FinalizedMutationConflict,
    #[error("service transaction journal sender {actual} does not match expected {expected}")]
    WrongSender { expected: Address, actual: Address },
}

/// Only observes the resident context. Nonce contention cannot clear RAM key ownership.
/// UpgradePrepare callers pass the candidate context, not the unchanged source context.
pub async fn reconcile_service_tx_v1<P: LifecycleChainPort>(
    port: &P,
    journal: &mut ServiceTxJournalV1,
    live: &InProcessTeeContext,
    expected_sender: Address,
) -> Result<ServiceTxReconciliationV1, ReconcileError<P::Error>> {
    let Some(record) = journal.load()? else {
        return Ok(ServiceTxReconciliationV1::Empty);
    };
    if record.sender != expected_sender {
        return Err(ReconcileError::WrongSender {
            expected: expected_sender,
            actual: record.sender,
        });
    }
    let at = port.finalized_checkpoint().await?;
    validate_checkpoint(&record, at, live)?;
    let selector = NodeBindingSelectorV1::Validator(record.sender);
    // Terminal journals already resolved the transaction; keep checking canonical Registry state.
    let receipt = if record.state == ServiceTxStateV1::Pending {
        port.finalized_receipt(at, record.tx_hash).await?
    } else {
        None
    };
    let view = port.registry_state(at, &selector).await?;
    validate_view(&view, at, live)?;
    validate_receipt(receipt, &record, at)?;
    let applied = mutation_applied(&record, &view);
    if receipt.is_some_and(|r| r.success) && !applied && record.state != ServiceTxStateV1::Completed
    {
        return Err(ReconcileError::FinalizedMutationConflict);
    }
    let manifest = live.manifest();
    // UpgradeCancel is an operator + CAS transaction (sender, chain, nodeIdHash,
    // expectedContextHash). Its target stays source_target for mutation_applied,
    // while any same-sender process owns a live or applied cancel.
    let operator_owned_cancel = record.kind == ServiceTxKindV1::UpgradeCancel
        && (applied
            || (view.candidate.is_some() && view.candidate == record.public_intent.candidate));
    let same_process = operator_owned_cancel
        || record.process_identity()? == (manifest.enclave_id()?, manifest.recipient_x25519.into());
    if let Some((outcome, state)) = terminal_outcome(&record, same_process, applied) {
        if !matches!(
            record.state,
            ServiceTxStateV1::Completed | ServiceTxStateV1::DiscardedLostIdentity
        ) {
            journal.finish(record.tx_hash, state, at)?;
        }
        return Ok(outcome);
    }
    let expired_renew = record.kind == ServiceTxKindV1::Renew
        && view
            .binding
            .as_ref()
            .is_some_and(|binding| at.timestamp >= binding.valid_until);
    reconcile_nonce(port, journal, &record, at, (same_process, expired_renew)).await
}

async fn reconcile_nonce<P: LifecycleChainPort>(
    port: &P,
    journal: &mut ServiceTxJournalV1,
    record: &ServiceTxRecordV1,
    at: LifecycleCheckpointV1,
    identity_and_expiry: (bool, bool),
) -> Result<ServiceTxReconciliationV1, ReconcileError<P::Error>> {
    let (same_process, expired_renew) = identity_and_expiry;
    let finalized = port
        .account_nonce(record.sender, LifecycleNonceTagV1::Finalized(at))
        .await?;
    let latest = port
        .account_nonce(record.sender, LifecycleNonceTagV1::Latest)
        .await?;
    let pending = port
        .account_nonce(record.sender, LifecycleNonceTagV1::Pending)
        .await?;
    if finalized > record.nonce {
        let (outcome, state) = if same_process {
            (
                ServiceTxReconciliationV1::RebuildAndResign {
                    nonce: finalized.max(latest).max(pending),
                },
                ServiceTxStateV1::RebuildRequired,
            )
        } else {
            (
                ServiceTxReconciliationV1::DiscardLostIdentity {
                    mutation_finalized: false,
                },
                ServiceTxStateV1::DiscardedLostIdentity,
            )
        };
        journal.finish(record.tx_hash, state, at)?;
        return Ok(outcome);
    }
    if expired_renew && latest <= record.nonce && pending <= record.nonce {
        if record.state != ServiceTxStateV1::RebuildRequired {
            journal.finish(record.tx_hash, ServiceTxStateV1::RebuildRequired, at)?;
        }
        return Ok(ServiceTxReconciliationV1::RebuildAndResign {
            nonce: record.nonce,
        });
    }
    // An external operator transaction (for example staking unjail) can sit above
    // an expired renew before it finalizes. That gap is not a checkpoint conflict.
    if latest > record.nonce || pending > record.nonce {
        return Ok(ServiceTxReconciliationV1::WaitForFinality {
            latest_nonce: latest,
            pending_nonce: pending,
        });
    }
    if record.state == ServiceTxStateV1::RebuildRequired {
        return Err(ReconcileError::CheckpointMismatch);
    }
    // A same-nonce replacement cannot execute alongside the old signed transaction.
    Ok(if same_process {
        ServiceTxReconciliationV1::ReplayExact
    } else {
        ServiceTxReconciliationV1::ReplaceLostIdentity {
            nonce: record.nonce,
            proof: LostIdentityProof::new(record),
        }
    })
}

fn validate_checkpoint<E: std::error::Error + 'static>(
    record: &ServiceTxRecordV1,
    at: LifecycleCheckpointV1,
    live: &InProcessTeeContext,
) -> Result<(), ReconcileError<E>> {
    let previous = record
        .reconciled_checkpoint
        .unwrap_or(record.finalized_checkpoint);
    let manifest = live.manifest();
    let chain_matches = (at.chain_id, at.genesis_hash)
        == (
            record.finalized_checkpoint.chain_id,
            record.finalized_checkpoint.genesis_hash,
        );
    let monotonic = at.height >= previous.height
        && at.timestamp >= previous.timestamp
        && (at.height != previous.height || at.hash == previous.hash);
    let live_chain = (
        alloy_primitives::U256::from(at.chain_id).to_be_bytes::<32>(),
        at.genesis_hash,
    ) == (manifest.chain_id, manifest.genesis_hash);
    let identity_matches = live_chain
        && record.public_intent.target.node_id_hash == manifest.node_id.node_id_hash()?;
    if ![
        chain_matches,
        monotonic,
        identity_matches,
        !at.hash.is_zero(),
        !at.state_root.is_zero(),
    ]
    .into_iter()
    .all(|v| v)
    {
        return Err(ReconcileError::CheckpointMismatch);
    }
    Ok(())
}

fn validate_view<E: std::error::Error + 'static>(
    view: &LifecycleRegistryStateV1,
    at: LifecycleCheckpointV1,
    live: &InProcessTeeContext,
) -> Result<(), ReconcileError<E>> {
    let pinned = (
        view.schedule.finalized_height,
        view.schedule.finalized_hash,
        view.schedule.finalized_timestamp,
    ) == (at.height, at.hash, at.timestamp);
    let chain = (view.policy.chain_id, view.policy.genesis_hash)
        == (live.manifest().chain_id, at.genesis_hash);
    if view.checkpoint != at || !pinned || !chain {
        return Err(ReconcileError::CheckpointMismatch);
    }
    Ok(())
}

fn validate_receipt<E: std::error::Error + 'static>(
    receipt: Option<LifecycleReceiptV1>,
    record: &ServiceTxRecordV1,
    at: LifecycleCheckpointV1,
) -> Result<(), ReconcileError<E>> {
    if let Some(receipt) = receipt {
        let canonical_head = receipt.block_height != at.height || receipt.block_hash == at.hash;
        if receipt.tx_hash != record.tx_hash
            || receipt.block_height > at.height
            || receipt.block_hash.is_zero()
            || !canonical_head
        {
            return Err(ReconcileError::CheckpointMismatch);
        }
    }
    Ok(())
}

fn terminal_outcome(
    record: &ServiceTxRecordV1,
    same_process: bool,
    applied: bool,
) -> Option<(ServiceTxReconciliationV1, ServiceTxStateV1)> {
    let settled = applied || record.state == ServiceTxStateV1::Completed;
    if record.state == ServiceTxStateV1::DiscardedLostIdentity || (!same_process && settled) {
        Some((
            ServiceTxReconciliationV1::DiscardLostIdentity {
                mutation_finalized: applied,
            },
            ServiceTxStateV1::DiscardedLostIdentity,
        ))
    } else if applied || record.state == ServiceTxStateV1::Completed {
        let onboarding = matches!(
            record.kind,
            ServiceTxKindV1::Register | ServiceTxKindV1::UpgradeTransition
        );
        let outcome = if applied && onboarding {
            ServiceTxReconciliationV1::IngestOnboarding
        } else {
            ServiceTxReconciliationV1::Completed
        };
        Some((outcome, ServiceTxStateV1::Completed))
    } else {
        None
    }
}

fn mutation_applied(record: &ServiceTxRecordV1, view: &LifecycleRegistryStateV1) -> bool {
    match record.kind {
        ServiceTxKindV1::Register | ServiceTxKindV1::Renew | ServiceTxKindV1::UpgradeTransition => {
            view.binding
                .as_ref()
                .is_some_and(|b| record.public_intent.target.matches(b))
        }
        ServiceTxKindV1::UpgradePrepare => {
            view.candidate == record.public_intent.candidate
                && view
                    .binding
                    .as_ref()
                    .is_some_and(|b| record.public_intent.target.matches(b))
        }
        ServiceTxKindV1::UpgradeCancel => {
            view.candidate.is_none()
                && view
                    .binding
                    .as_ref()
                    .is_some_and(|b| record.public_intent.target.matches(b))
        }
    }
}

#[cfg(test)]
mod tests;
