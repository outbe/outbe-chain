//! Reconcile finality before any replay, nonce rebuild, or process-identity discard.
use super::{rpc_port::*, tx_journal::*};
use crate::{in_process::InProcessTeeContext, registry_view::NodeBindingSelectorV1};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceTxReconciliationV1 {
    Empty,
    Completed,
    IngestOnboarding,
    DiscardLostIdentity {
        mutation_finalized: bool,
    },
    ReplayExact,
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
}

/// Only observes the resident context. Nonce contention cannot clear RAM key ownership.
pub async fn reconcile_service_tx_v1<P: LifecycleChainPort>(
    port: &P,
    journal: &mut ServiceTxJournalV1,
    live: &InProcessTeeContext,
) -> Result<ServiceTxReconciliationV1, ReconcileError<P::Error>> {
    let Some(record) = journal.load()? else {
        return Ok(ServiceTxReconciliationV1::Empty);
    };
    let at = port.finalized_checkpoint().await?;
    validate_checkpoint(&record, at, live)?;
    let selector = NodeBindingSelectorV1::Validator(record.sender);
    // Receipt and canonical Registry state are both consulted before any decision.
    let receipt = port.finalized_receipt(at, record.tx_hash).await?;
    let view = port.registry_state(at, &selector).await?;
    validate_view(&view, at, live)?;
    validate_receipt(receipt, &record, at)?;
    let applied = mutation_applied(&record, &view);
    if receipt.is_some_and(|r| r.success) && !applied && record.state != ServiceTxStateV1::Completed
    {
        return Err(ReconcileError::FinalizedMutationConflict);
    }
    if let Some((outcome, state)) = terminal_outcome(&record, live, applied)? {
        journal.finish(record.tx_hash, state, at)?;
        return Ok(outcome);
    }
    reconcile_nonce(port, journal, &record, at).await
}

async fn reconcile_nonce<P: LifecycleChainPort>(
    port: &P,
    journal: &mut ServiceTxJournalV1,
    record: &ServiceTxRecordV1,
    at: LifecycleCheckpointV1,
) -> Result<ServiceTxReconciliationV1, ReconcileError<P::Error>> {
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
        journal.finish(record.tx_hash, ServiceTxStateV1::RebuildRequired, at)?;
        return Ok(ServiceTxReconciliationV1::RebuildAndResign {
            nonce: finalized.max(latest).max(pending),
        });
    }
    if record.state == ServiceTxStateV1::RebuildRequired {
        return Err(ReconcileError::CheckpointMismatch);
    }
    if latest > record.nonce || pending > record.nonce {
        return Ok(ServiceTxReconciliationV1::WaitForFinality {
            latest_nonce: latest,
            pending_nonce: pending,
        });
    }
    Ok(ServiceTxReconciliationV1::ReplayExact)
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

fn terminal_outcome<E: std::error::Error + 'static>(
    record: &ServiceTxRecordV1,
    live: &InProcessTeeContext,
    applied: bool,
) -> Result<Option<(ServiceTxReconciliationV1, ServiceTxStateV1)>, ReconcileError<E>> {
    let manifest = live.manifest();
    let same_process = record.public_intent.target.enclave_id == manifest.enclave_id()?
        && record.public_intent.target.recipient_x25519.as_slice() == manifest.recipient_x25519;
    let terminal = if !same_process || record.state == ServiceTxStateV1::DiscardedLostIdentity {
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
    };
    Ok(terminal)
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
