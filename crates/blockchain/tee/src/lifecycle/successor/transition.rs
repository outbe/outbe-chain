use super::candidate::*;
use super::*;
use crate::{
    in_process::{admission::FinalizedOnboardingV1, InProcessTeeContext},
    lifecycle::{renewal::prepare::*, *},
    upgrade_transfer::UpgradeKeyProofV1,
};
use alloy_primitives::{B256, U256};
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
    let (nonce, lost_proof) =
        match fresh_nonce::<P, S>(port, signer, journal, live, ServiceTxKindV1::UpgradeCancel)
            .await?
        {
            FreshNonceV1::AwaitingFinality => return Ok(SuccessorStep::AwaitingFinality),
            FreshNonceV1::Fresh(nonce) => (nonce, None),
            FreshNonceV1::Replacing { nonce, proof } => (nonce, Some(proof)),
        };
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
    persist_record::<P, S>(journal, record.clone(), lost_proof)?;
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
    // The proof was assembled at its own finalized header. Finality may advance
    // before this call. The candidate is still checked against `at` above.
    let proof_at = upgrade_proof_checkpoint::<P, S>(proof, at)?;
    let supplied = port
        .upgrade_key(proof_at, &attempt.context, proof)
        .await
        .map_err(RenewalError::from)?;
    ingest_candidate_key_v1::<P, S>(candidate, attempt, &view, &supplied)?;
    Ok(SuccessorStep::KeyReady)
}

/// Header checkpoint sealed inside `UpgradeKeyProofV1`.
///
/// The header may lag `current` when finality advanced after the proof was built.
/// `base_fee_per_gas` is the header's own fee: `upgrade_key` checks this
/// checkpoint against block H1, including the historical base fee.
/// A header ahead of finality, a missing base fee, a truncated header, or a
/// same-height hash, `state_root`, or base-fee disagreement fails closed.
fn upgrade_proof_checkpoint<P: LifecycleChainPort, S: LifecycleSignerV1>(
    proof: &UpgradeKeyProofV1,
    current: LifecycleCheckpointV1,
) -> SuccessorResult<LifecycleCheckpointV1, P, S> {
    use alloy_rlp::Decodable as _;
    let witness =
        crate::finalized_admission::FinalizedAdmissionWitnessV1::decode_canonical(&proof.admission)
            .map_err(|source| {
                RenewalError::<P::Error, S::Error>::UpgradeProofCheckpoint(
                    UpgradeProofCheckpointError::Witness(source),
                )
            })?;
    let mut rlp = witness.admission.header.as_slice();
    let header = alloy_consensus::Header::decode(&mut rlp).map_err(|source| {
        RenewalError::<P::Error, S::Error>::UpgradeProofCheckpoint(
            UpgradeProofCheckpointError::Header(source),
        )
    })?;
    let hash = header.hash_slow();
    let Some(base_fee) = header.base_fee_per_gas.map(U256::from) else {
        return Err(RenewalError::<P::Error, S::Error>::UpgradeProofCheckpoint(
            UpgradeProofCheckpointError::MissingBaseFee,
        )
        .into());
    };
    let same_height = header.number == current.height;
    let head_matches = (hash, header.state_root, base_fee)
        == (current.hash, current.state_root, current.base_fee_per_gas);
    let intact = [
        rlp.is_empty(),
        !hash.is_zero(),
        !header.state_root.is_zero(),
        header.number <= current.height,
        header.timestamp <= current.timestamp,
        !same_height || head_matches,
    ]
    .into_iter()
    .all(|flag| flag);
    if !intact {
        return Err(RenewalError::<P::Error, S::Error>::BindingMismatch.into());
    }
    Ok(LifecycleCheckpointV1 {
        height: header.number,
        hash,
        state_root: header.state_root,
        timestamp: header.timestamp,
        base_fee_per_gas: base_fee,
        chain_id: current.chain_id,
        genesis_hash: current.genesis_hash,
    })
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
    // An already applied transition is settled before the staged successor is read.
    // A later policy, including a different policy hash or a late activation, must
    // not reject or resend that journal.
    if let Some(step) = finalized_recorded_transition(port, signer, candidate, journal, b).await? {
        return Ok(step);
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
    require_prepared_candidate::<P, S>(&view, attempt, candidate)?;
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
    if let Some(step) =
        resume_pending_candidate_transition(port, signer, candidate, journal, &intent).await?
    {
        return Ok(step);
    }
    submit_fresh_transition::<P, S>(port, signer, candidate, journal, (at, policy, &mut intent))
        .await
}

async fn submit_fresh_transition<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    candidate: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    submission: (
        LifecycleCheckpointV1,
        &TeePolicyV1,
        &mut RegistrationIntentV1,
    ),
) -> SuccessorResult<SuccessorStep, P, S> {
    let (at, policy, intent) = submission;
    let (evidence, signature) =
        transition_evidence::<P, S>(candidate, intent, policy, at.timestamp)?;
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
    let (nonce, lost_proof) = match fresh_nonce::<P, S>(
        port,
        signer,
        journal,
        candidate,
        ServiceTxKindV1::UpgradeTransition,
    )
    .await?
    {
        FreshNonceV1::AwaitingFinality => return Ok(SuccessorStep::AwaitingFinality),
        FreshNonceV1::Fresh(nonce) => (nonce, None),
        FreshNonceV1::Replacing { nonce, proof } => (nonce, Some(proof)),
    };
    let record = signed_record::<P, S>(
        signer,
        at,
        nonce,
        ServiceCallV1 {
            gas_limit: gas,
            calldata,
            kind: ServiceTxKindV1::UpgradeTransition,
            intent: ServiceTxIntentV1 {
                target: target_v1(intent).map_err(RenewalError::from)?,
                candidate: None,
            },
        },
    )?;
    persist_record::<P, S>(journal, record.clone(), lost_proof)?;
    send::<P, S>(port, &record, false).await
}

fn require_prepared_candidate<P: LifecycleChainPort, S: LifecycleSignerV1>(
    view: &LifecycleRegistryStateV1,
    attempt: &SuccessorAttemptV1,
    candidate: &InProcessTeeContext,
) -> SuccessorResult<(), P, S> {
    if view.candidate.is_none()
        || reconcile_candidate_v1(view, Some(attempt), Some(candidate)) != SuccessorStep::Prepared
    {
        return Err(SuccessorError::CandidateChanged);
    }
    Ok(())
}

/// A journaled transition that already matches the canonical binding is settled.
/// Candidate staging and the fresh-id check describe the pre-image, not this binding.
async fn finalized_recorded_transition<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    candidate: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    binding: &crate::registry_view::RenewalBindingV1,
) -> SuccessorResult<Option<SuccessorStep>, P, S> {
    let Some(record) = journal.load().map_err(RenewalError::from)? else {
        return Ok(None);
    };
    let applied = record.kind == ServiceTxKindV1::UpgradeTransition
        && record.public_intent.target.matches(binding);
    if !applied {
        return Ok(None);
    }
    reconcile_successor_v1(port, signer, candidate, journal)
        .await
        .map(Some)
}

/// A pending transition is tracked by candidate identity, not by the source binding.
/// The journal target must equal the signed calldata. A shorter signed lease still
/// matches a longer new request. A terminal `RebuildRequired` transition is re-signed.
async fn resume_pending_candidate_transition<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    candidate: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    intent: &RegistrationIntentV1,
) -> SuccessorResult<Option<SuccessorStep>, P, S> {
    let Some(record) = journal.load().map_err(RenewalError::from)? else {
        return Ok(None);
    };
    let pending = record.kind == ServiceTxKindV1::UpgradeTransition
        && record.state == ServiceTxStateV1::Pending;
    let displaced = record.kind == ServiceTxKindV1::UpgradeTransition
        && record.state == ServiceTxStateV1::RebuildRequired;
    if !pending && !displaced {
        return Ok(None);
    }
    if !transition_targets_candidate::<P, S>(&record, intent)? {
        return Err(SuccessorError::CandidateMismatch);
    }
    if displaced {
        return Ok(None);
    }
    reconcile_successor_v1(port, signer, candidate, journal)
        .await
        .map(Some)
}

fn transition_targets_candidate<P: LifecycleChainPort, S: LifecycleSignerV1>(
    record: &ServiceTxRecordV1,
    intent: &RegistrationIntentV1,
) -> SuccessorResult<bool, P, S> {
    let signed = super::recovery::decode_intent(record).map_err(RenewalError::from)?;
    let signed_target = target_v1(&signed).map_err(RenewalError::from)?;
    let requested = target_v1(intent).map_err(RenewalError::from)?;
    Ok(record.public_intent.target == signed_target
        && signed_lease_fits(&signed_target, &requested))
}

/// Clamp is only `signed.valid_until <= requested`. Every other candidate field matches.
fn signed_lease_fits(signed: &ServiceBindingTargetV1, requested: &ServiceBindingTargetV1) -> bool {
    let mut identity = requested.clone();
    identity.valid_until = signed.valid_until;
    signed == &identity && signed.valid_until <= requested.valid_until
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
