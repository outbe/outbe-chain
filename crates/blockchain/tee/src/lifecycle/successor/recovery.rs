//! Finalized-first replay/rebuild routing for the shared public service journal.
use super::candidate::send;
use super::*;
use crate::{in_process::InProcessTeeContext, lifecycle::*};

/// Supply the candidate for prepare/transition, or the current resident source for cancel.
/// A rebuild result permits fresh evidence at that nonce; it never permits clearing RAM keys.
pub async fn reconcile_successor_v1<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    live: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
) -> SuccessorResult<SuccessorStep, P, S> {
    let record = journal.load().map_err(RenewalError::from)?;
    let Some(record) = record else {
        return Ok(SuccessorStep::Prepared);
    };
    if record.sender != signer.address() {
        return Err(SuccessorError::PendingOperation);
    }
    if let Some(step) = historical_cancel::<P, S>(port, signer, live, journal, &record).await? {
        return Ok(step);
    }
    let result = reconcile_service_tx_v1(port, journal, live, signer.address())
        .await
        .map_err(RenewalError::from)?;
    match result {
        ServiceTxReconciliationV1::ReplayExact => {
            if !matches!(
                record.kind,
                ServiceTxKindV1::UpgradePrepare
                    | ServiceTxKindV1::UpgradeTransition
                    | ServiceTxKindV1::UpgradeCancel
            ) {
                return Err(SuccessorError::PendingOperation);
            }
            let at = port
                .finalized_checkpoint()
                .await
                .map_err(RenewalError::from)?;
            let view = port
                .registry_state(
                    at,
                    &crate::registry_view::NodeBindingSelectorV1::Validator(signer.address()),
                )
                .await
                .map_err(RenewalError::from)?;
            crate::lifecycle::renewal::prepare::validate_view::<P::Error, S::Error>(
                &view, at, live,
            )?;
            if record.kind != ServiceTxKindV1::UpgradeCancel {
                let intent = decode_intent(&record).map_err(RenewalError::from)?;
                if at.timestamp >= intent.requested_valid_until {
                    return Err(SuccessorError::LeaseWindow);
                }
            }
            send::<P, S>(port, &record, true).await
        }
        ServiceTxReconciliationV1::WaitForFinality { .. } => Ok(SuccessorStep::AwaitingFinality),
        ServiceTxReconciliationV1::ReplaceLostIdentity { nonce, .. }
        | ServiceTxReconciliationV1::RebuildAndResign { nonce } => {
            Ok(SuccessorStep::RebuildRequired { nonce })
        }
        ServiceTxReconciliationV1::DiscardLostIdentity { .. } => {
            let at = port
                .finalized_checkpoint()
                .await
                .map_err(RenewalError::from)?;
            let view = port
                .registry_state(
                    at,
                    &crate::registry_view::NodeBindingSelectorV1::Validator(signer.address()),
                )
                .await
                .map_err(RenewalError::from)?;
            Ok(match view.candidate.filter(|c| !c.context_hash.is_zero()) {
                Some(c) => SuccessorStep::CancelRequired {
                    context_hash: c.context_hash,
                },
                None => SuccessorStep::LateRecoveryRequired,
            })
        }
        ServiceTxReconciliationV1::Empty
        | ServiceTxReconciliationV1::Completed
        | ServiceTxReconciliationV1::IngestOnboarding => {
            Ok(if record.kind == ServiceTxKindV1::UpgradePrepare {
                SuccessorStep::Prepared
            } else {
                SuccessorStep::Finalized
            })
        }
    }
}

pub(crate) fn decode_intent(
    record: &ServiceTxRecordV1,
) -> Result<outbe_primitives::tee_attestation_v1::RegistrationIntentV1, ServiceTxJournalError> {
    use alloy_consensus::{transaction::RlpEcdsaDecodableTx as _, TxEip1559};
    use alloy_sol_types::SolCall as _;
    use outbe_primitives::{
        tee_attestation_v1::AttestationEvidenceV1, tee_registry_abi_v1::ITeeRegistryV1,
    };
    let mut raw = &record.raw_signed_tx[1..];
    let tx = TxEip1559::rlp_decode_signed(&mut raw)?;
    let evidence = match record.kind {
        ServiceTxKindV1::UpgradePrepare => {
            ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&tx.tx().input)?.evidence
        }
        ServiceTxKindV1::UpgradeTransition => {
            ITeeRegistryV1::transitionEnclaveMeasurementCall::abi_decode(&tx.tx().input)?.evidence
        }
        _ => return Err(ServiceTxJournalError::InvalidIntent),
    };
    Ok(AttestationEvidenceV1::decode_canonical(&evidence)?
        .intent()
        .clone())
}

/// A successful historical prepare can be superseded by a finalized source rejoin.
/// Require the exact surviving candidate and canonical successful receipt before settling it.
pub(crate) async fn settle_historical_prepare<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    journal: &mut ServiceTxJournalV1,
    view: &LifecycleRegistryStateV1,
) -> SuccessorResult<(), P, S> {
    let Some(record) = journal.load().map_err(RenewalError::from)? else {
        return Ok(());
    };
    let current_source = view.binding.as_ref().map(|b| b.binding_id);
    let historical = record.kind == ServiceTxKindV1::UpgradePrepare
        && record.state == ServiceTxStateV1::Pending
        && current_source.is_some_and(|id| id != record.public_intent.target.binding_id);
    if !historical {
        return Ok(());
    }
    let exact = view.candidate == record.public_intent.candidate
        && view
            .candidate
            .is_some_and(|c| c.source_binding_id == record.public_intent.target.binding_id);
    if !exact {
        return Err(SuccessorError::CandidateChanged);
    }
    let at = view.checkpoint;
    let receipt = port
        .finalized_receipt(at, record.tx_hash)
        .await
        .map_err(RenewalError::from)?;
    let canonical = receipt.is_some_and(|r| {
        let identity = r.success && r.tx_hash == record.tx_hash;
        let height = r.block_height <= at.height && !r.block_hash.is_zero();
        let head = r.block_height != at.height || r.block_hash == at.hash;
        identity && height && head
    });
    if !canonical {
        return Err(SuccessorError::PendingOperation);
    }
    journal
        .finish(record.tx_hash, ServiceTxStateV1::Completed, at)
        .map_err(RenewalError::from)?;
    Ok(())
}

async fn historical_cancel<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    live: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    record: &ServiceTxRecordV1,
) -> SuccessorResult<Option<SuccessorStep>, P, S> {
    if record.kind == ServiceTxKindV1::UpgradePrepare {
        let at = port
            .finalized_checkpoint()
            .await
            .map_err(RenewalError::from)?;
        let view = port
            .registry_state(
                at,
                &crate::registry_view::NodeBindingSelectorV1::Validator(signer.address()),
            )
            .await
            .map_err(RenewalError::from)?;
        crate::lifecycle::renewal::prepare::validate_view::<P::Error, S::Error>(&view, at, live)?;
        settle_historical_prepare::<P, S>(port, journal, &view).await?;
        if view
            .binding
            .as_ref()
            .is_some_and(|b| b.binding_id != record.public_intent.target.binding_id)
        {
            if let Some(candidate) = view.candidate {
                return Ok(Some(SuccessorStep::CancelRequired {
                    context_hash: candidate.context_hash,
                }));
            }
        }
    }
    Ok(None)
}
