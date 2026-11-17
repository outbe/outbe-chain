use super::candidate::*;
use super::*;
use crate::{
    in_process::{admission::FinalizedOnboardingV1, InProcessTeeContext},
    lifecycle::{renewal::prepare::*, *},
    upgrade_transfer::UpgradeKeyProofV1,
};
use alloy_primitives::B256;
use alloy_sol_types::SolCall as _;
use outbe_primitives::{tee_attestation_v1::*, tee_registry_abi_v1::ITeeRegistryV1};

/// A missing or changed RAM candidate is cancelled, never silently adopted after restart.
pub fn reconcile_candidate_v1(
    view: &LifecycleRegistryStateV1,
    attempt: Option<&SuccessorAttemptV1>,
    candidate: Option<&InProcessTeeContext>,
) -> SuccessorStep {
    let Some(pending) = view.candidate.filter(|c| !c.context_hash.is_zero()) else {
        return SuccessorStep::Prepared;
    };
    let source_matches = view
        .binding
        .as_ref()
        .is_some_and(|b| b.binding_id == pending.source_binding_id);
    let current = attempt.zip(candidate).is_some_and(|(a, c)| {
        [
            a.context.context_hash() == pending.context_hash,
            a.prepare_intent.upgrade_target_hash().ok() == Some(pending.target_hash),
            (
                a.prepare_intent.requested_valid_until,
                a.prepare_intent.transition_nonce,
            ) == (pending.valid_until, pending.nonce),
            c.manifest().enclave_id().ok() == Some(a.prepare_intent.enclave_id),
            c.manifest().recipient_x25519 == a.prepare_intent.recipient_x25519,
        ]
        .into_iter()
        .all(|v| v)
    });
    if !source_matches || !current || pending.valid_until <= view.checkpoint.timestamp {
        SuccessorStep::CancelRequired {
            context_hash: pending.context_hash,
        }
    } else {
        SuccessorStep::Prepared
    }
}

/// Cancel by compare-and-swap context hash. A new source binding is pinned in the journal.
pub async fn cancel_successor_v1<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    live: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    expected: B256,
) -> SuccessorResult<SuccessorStep, P, S> {
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
    validate_view::<P::Error, S::Error>(&view, at, live)?;
    let source = view
        .binding
        .as_ref()
        .ok_or(SuccessorError::CandidateMismatch)?;
    let pending = view
        .candidate
        .filter(|c| c.context_hash == expected && !expected.is_zero())
        .ok_or(SuccessorError::CandidateChanged)?;
    let calldata = ITeeRegistryV1::cancelEnclaveUpgradeCall {
        nodeIdHash: source.node_id_hash,
        expectedContextHash: expected,
    }
    .abi_encode();
    // Cancellation needs no QVL; the normative prepare ceiling safely bounds its storage writes.
    let gas = registry_gas::<P, S>(
        RegistryMutatorV1::PrepareEnclaveUpgrade,
        &calldata,
        &[],
        &view.policy,
    )?;
    super::recovery::settle_historical_prepare::<P, S>(port, journal, &view).await?;
    let nonce =
        fresh_nonce::<P, S>(port, signer, journal, live, ServiceTxKindV1::UpgradeCancel).await?;
    let record = signed_record::<P, S>(
        signer,
        at,
        nonce,
        ServiceCallV1 {
            gas_limit: gas,
            calldata,
            kind: ServiceTxKindV1::UpgradeCancel,
            intent: ServiceTxIntentV1 {
                target: source_target(source),
                candidate: Some(pending),
            },
        },
    )?;
    journal.begin(record.clone()).map_err(RenewalError::from)?;
    send::<P, S>(port, &record, false).await
}

/// Fetch only after finalized candidate identity matches. Import verifies BLS and MPT from genesis.
pub async fn provision_successor_v1<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    candidate: &InProcessTeeContext,
    attempt: &SuccessorAttemptV1,
    proof: &UpgradeKeyProofV1,
) -> SuccessorResult<SuccessorStep, P, S> {
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
    validate_view::<P::Error, S::Error>(&view, at, candidate)?;
    if reconcile_candidate_v1(&view, Some(attempt), Some(candidate)) != SuccessorStep::Prepared
        || view.candidate.is_none()
    {
        return Err(SuccessorError::CandidateChanged);
    }
    if candidate.offer_key_ready().map_err(RenewalError::from)? {
        require_network_key::<P, S>(candidate, &view)?;
        return Ok(SuccessorStep::KeyReady);
    }
    proof.validate().map_err(RenewalError::Collateral)?;
    let supplied = port
        .upgrade_key(at, &attempt.context, proof)
        .await
        .map_err(RenewalError::from)?;
    ingest_candidate_key_v1::<P, S>(candidate, attempt, &view, &supplied)?;
    Ok(SuccessorStep::KeyReady)
}

pub(crate) fn ingest_candidate_key_v1<P: LifecycleChainPort, S: LifecycleSignerV1>(
    candidate: &InProcessTeeContext,
    attempt: &SuccessorAttemptV1,
    view: &LifecycleRegistryStateV1,
    supplied: &LifecycleOnboardingProofV1,
) -> SuccessorResult<(), P, S> {
    supplied
        .proof
        .validate()
        .map_err(RenewalError::Collateral)?;
    let authorization = candidate
        .authorization_witness()
        .map_err(RenewalError::from)?;
    let transitions = supplied
        .proof
        .committee_transitions
        .iter()
        .map(|b| b.to_vec())
        .collect::<Vec<_>>();
    candidate
        .ingest_upgrade_key_v1(FinalizedOnboardingV1 {
            artifact: &supplied.artifact,
            intent: &attempt.prepare_intent,
            authorization: &authorization,
            anchor_outcome: &supplied.proof.anchor_outcome,
            committee_transitions: &transitions,
            admission_witness: &supplied.proof.admission,
            expected_network_encryption_public: view.network_encryption_public_key.0,
            expected_key_epoch: view.key_epoch,
            expected_network_encryption_epoch: view.network_encryption_epoch,
        })
        .map_err(RenewalError::from)?;
    require_network_key::<P, S>(candidate, view)
}

fn require_network_key<P: LifecycleChainPort, S: LifecycleSignerV1>(
    candidate: &InProcessTeeContext,
    view: &LifecycleRegistryStateV1,
) -> SuccessorResult<(), P, S> {
    if (
        candidate
            .network_encryption_public()
            .map_err(RenewalError::from)?,
        candidate.key_epoch().map_err(RenewalError::from)?,
        candidate
            .network_encryption_epoch()
            .map_err(RenewalError::from)?,
    ) != (
        view.network_encryption_public_key.0,
        view.key_epoch,
        view.network_encryption_epoch,
    ) {
        return Err(SuccessorError::NetworkKeyMismatch);
    }
    Ok(())
}

/// Fresh counters and a new key-ready proof are signed at every transition preparation.
/// The strict active-upgrade branch remains valid after activation and source lease expiry.
pub async fn transition_successor_v1<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    candidate: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    attempt: &SuccessorAttemptV1,
) -> SuccessorResult<SuccessorStep, P, S> {
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
    validate_view::<P::Error, S::Error>(&view, at, candidate)?;
    let b = view
        .binding
        .as_ref()
        .ok_or(SuccessorError::CandidateMismatch)?;
    if let Some(record) = journal.load().map_err(RenewalError::from)? {
        if record.kind == ServiceTxKindV1::UpgradeTransition
            && record.public_intent.target.matches(b)
        {
            return reconcile_successor_v1(port, signer, candidate, journal).await;
        }
    }
    let policy = successor_policy::<P, S>(&view)?;
    if policy.policy_hash().map_err(RenewalError::from)? != attempt.prepare_intent.policy_hash {
        return Err(SuccessorError::PolicyMismatch);
    }
    let late = at.height >= policy.activation_height;
    if late
        && view
            .active_upgrade
            .is_none_or(|u| u.successor_policy_hash != attempt.prepare_intent.policy_hash)
    {
        return Ok(SuccessorStep::LateRecoveryRequired);
    }
    if view.candidate.is_none()
        || reconcile_candidate_v1(&view, Some(attempt), Some(candidate)) != SuccessorStep::Prepared
    {
        return Err(SuccessorError::CandidateChanged);
    }
    validate_candidate::<P, S>(candidate, b, attempt.prepare_intent.binding_id)?;
    require_network_key::<P, S>(candidate, &view)?;
    let mut intent = transition_intent::<P, S>(
        candidate,
        b,
        policy,
        attempt.prepare_intent.binding_id,
        attempt.prepare_intent.requested_valid_until,
    )?;
    validate_lease::<P, S>(&intent, policy, at.timestamp)?;
    let (evidence, signature) =
        transition_evidence::<P, S>(candidate, &mut intent, policy, at.timestamp)?;
    let node = signer
        .sign_node_hash(intent.intent_hash().map_err(RenewalError::from)?)
        .map_err(RenewalError::Signing)?;
    if !intent.verify_node_signature(&node) {
        return Err(SuccessorError::CandidateMismatch);
    }
    let calldata = ITeeRegistryV1::transitionEnclaveMeasurementCall {
        evidence: evidence.clone().into(),
        nodeSignature: node.to_vec().into(),
        enclaveSignature: signature.to_vec().into(),
    }
    .abi_encode();
    let gas = registry_gas::<P, S>(
        RegistryMutatorV1::TransitionEnclaveMeasurement,
        &calldata,
        &evidence,
        policy,
    )?;
    let nonce = fresh_nonce::<P, S>(
        port,
        signer,
        journal,
        candidate,
        ServiceTxKindV1::UpgradeTransition,
    )
    .await?;
    let record = signed_record::<P, S>(
        signer,
        at,
        nonce,
        ServiceCallV1 {
            gas_limit: gas,
            calldata,
            kind: ServiceTxKindV1::UpgradeTransition,
            intent: ServiceTxIntentV1 {
                target: target_v1(&intent).map_err(RenewalError::from)?,
                candidate: None,
            },
        },
    )?;
    journal.begin(record.clone()).map_err(RenewalError::from)?;
    send::<P, S>(port, &record, false).await
}

/// The legacy transition contract may shorten a lease to fit signed collateral, then re-sign.
fn transition_evidence<P: LifecycleChainPort, S: LifecycleSignerV1>(
    candidate: &InProcessTeeContext,
    intent: &mut RegistrationIntentV1,
    policy: &TeePolicyV1,
    now: u64,
) -> SuccessorResult<(Vec<u8>, [u8; 64]), P, S> {
    match evidence_v1::<P::Error, S::Error>(candidate, intent, policy, now) {
        Err(RenewalError::CollateralWindow {
            issue_floor,
            expiration_ceiling,
        }) if issue_floor <= now => {
            intent.requested_valid_until =
                clamped_deadline::<P, S>(now, policy, expiration_ceiling)?;
            evidence_v1::<P::Error, S::Error>(candidate, intent, policy, now).map_err(Into::into)
        }
        result => result.map_err(Into::into),
    }
}

pub(super) fn clamped_deadline<P: LifecycleChainPort, S: LifecycleSignerV1>(
    now: u64,
    policy: &TeePolicyV1,
    expiration: u64,
) -> SuccessorResult<u64, P, S> {
    let ceiling = expiration
        .checked_sub(policy.collateral_margin)
        .ok_or(SuccessorError::LeaseWindow)?;
    let minimum = now
        .checked_add(policy.minimum_lease)
        .ok_or(SuccessorError::LeaseWindow)?;
    if ceiling < minimum {
        return Err(SuccessorError::LeaseWindow);
    }
    Ok(ceiling)
}
