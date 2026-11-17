use super::*;
use crate::lifecycle::{renewal::prepare::*, *};
use crate::{in_process::InProcessTeeContext, registry_view::RenewalBindingV1};
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::*;

/// One candidate attempt lives only in RAM. Restart recovery uses signed public journal data.
#[derive(Clone, Debug)]
pub struct SuccessorAttemptV1 {
    pub prepare_intent: RegistrationIntentV1,
    pub source: RenewalBindingV1,
    pub context: crate::dcap_protocol::DcapOnboardingContextV1,
}

/// Build from one finalized snapshot; the counter is read even when no candidate exists.
pub async fn prepare_successor_v1<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    candidate: &InProcessTeeContext,
    journal: &mut ServiceTxJournalV1,
    lease: SuccessorLeaseV1,
) -> SuccessorResult<(SuccessorAttemptV1, SuccessorStep), P, S> {
    let SuccessorLeaseV1 {
        binding_id,
        valid_until,
    } = lease;
    let at = port
        .finalized_checkpoint()
        .await
        .map_err(RenewalError::from)?;
    let selector = crate::registry_view::NodeBindingSelectorV1::Validator(signer.address());
    let view = port
        .registry_state(at, &selector)
        .await
        .map_err(RenewalError::from)?;
    validate_view::<P::Error, S::Error>(&view, at, candidate)?;
    let source = view
        .binding
        .as_ref()
        .ok_or(SuccessorError::CandidateMismatch)?;
    let policy = successor_policy::<P, S>(&view)?;
    validate_candidate::<P, S>(candidate, source, binding_id)?;
    let policy_hash = policy.policy_hash().map_err(RenewalError::from)?;
    let strict_late = at.height >= policy.activation_height
        && view
            .active_upgrade
            .is_some_and(|u| u.successor_policy_hash == policy_hash);
    if at.timestamp >= source.valid_until && !strict_late {
        return Err(SuccessorError::SourceLeaseExpiredNeedsRejoin);
    }
    if let Some(current) = view.candidate.filter(|c| !c.context_hash.is_zero()) {
        return Err(if current.valid_until > at.timestamp {
            SuccessorError::PendingOperation
        } else {
            SuccessorError::CandidateChanged
        });
    }
    let previous = port
        .pending_upgrade_nonce(at, &selector)
        .await
        .map_err(RenewalError::from)?;
    let mut intent = transition_intent::<P, S>(candidate, source, policy, binding_id, valid_until)?;
    intent.operation = AttestationOperationV1::PrepareEnclaveUpgrade;
    intent.transition_nonce = previous
        .checked_add(1)
        .ok_or(RenewalError::CounterOverflow)?;
    validate_lease::<P, S>(&intent, policy, at.timestamp)?;
    let context = onboarding_context::<P, S>(&intent, &view)?;
    let (evidence, signature) =
        evidence_v1::<P::Error, S::Error>(candidate, &intent, policy, at.timestamp)?;
    let node = signer
        .sign_node_hash(intent.intent_hash().map_err(RenewalError::from)?)
        .map_err(RenewalError::Signing)?;
    if !intent.verify_node_signature(&node) {
        return Err(SuccessorError::CandidateMismatch);
    }
    use alloy_sol_types::SolCall as _;
    let calldata =
        outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::prepareEnclaveUpgradeCall {
            evidence: evidence.clone().into(),
            nodeSignature: node.to_vec().into(),
            enclaveSignature: signature.to_vec().into(),
        }
        .abi_encode();
    let gas = registry_gas::<P, S>(
        RegistryMutatorV1::PrepareEnclaveUpgrade,
        &calldata,
        &evidence,
        policy,
    )?;
    let target = source_target(source);
    let expected = LifecycleUpgradeCandidateV1 {
        context_hash: context.context_hash(),
        valid_until,
        source_binding_id: source.binding_id,
        target_hash: intent.upgrade_target_hash().map_err(RenewalError::from)?,
        nonce: intent.transition_nonce,
    };
    let nonce = fresh_nonce::<P, S>(
        port,
        signer,
        journal,
        candidate,
        ServiceTxKindV1::UpgradePrepare,
    )
    .await?;
    let record = signed_record::<P, S>(
        signer,
        at,
        nonce,
        ServiceCallV1 {
            gas_limit: gas,
            calldata,
            kind: ServiceTxKindV1::UpgradePrepare,
            intent: ServiceTxIntentV1 {
                target,
                candidate: Some(expected),
            },
        },
    )?;
    journal.begin(record.clone()).map_err(RenewalError::from)?;
    let outcome = send::<P, S>(port, &record, false).await?;
    Ok((
        SuccessorAttemptV1 {
            prepare_intent: intent,
            source: source.clone(),
            context,
        },
        outcome,
    ))
}

pub(crate) fn successor_policy<P: LifecycleChainPort, S: LifecycleSignerV1>(
    view: &LifecycleRegistryStateV1,
) -> SuccessorResult<&TeePolicyV1, P, S> {
    let successor = view
        .successor_policy
        .as_ref()
        .ok_or(SuccessorError::PolicyMismatch)?;
    let p = &successor.policy;
    let active = view.policy.policy_hash().map_err(RenewalError::from)?;
    let hash = p.policy_hash().map_err(RenewalError::from)?;
    if (successor.finalized_height, successor.finalized_hash)
        != (view.checkpoint.height, view.checkpoint.hash)
        || (p.chain_id, p.genesis_hash) != (view.policy.chain_id, view.policy.genesis_hash)
        || (p.predecessor_policy_hash != active && hash != active)
    {
        return Err(SuccessorError::PolicyMismatch);
    }
    Ok(p)
}

pub(crate) fn validate_candidate<P: LifecycleChainPort, S: LifecycleSignerV1>(
    candidate: &InProcessTeeContext,
    source: &RenewalBindingV1,
    binding: B256,
) -> SuccessorResult<(), P, S> {
    let m = candidate.manifest();
    let node_matches = (
        m.node_id.node_id_hash().map_err(RenewalError::from)?,
        m.node_host_authorization_hash()
            .map_err(RenewalError::from)?,
    ) == (source.node_id_hash, source.node_host_authorization_hash);
    let fresh_ids = m.enclave_id().map_err(RenewalError::from)? != source.enclave_id
        && !binding.is_zero()
        && binding != source.binding_id;
    if !node_matches || !fresh_ids {
        return Err(SuccessorError::CandidateMismatch);
    }
    Ok(())
}

pub(crate) fn transition_intent<P: LifecycleChainPort, S: LifecycleSignerV1>(
    candidate: &InProcessTeeContext,
    b: &RenewalBindingV1,
    p: &TeePolicyV1,
    binding: B256,
    valid_until: u64,
) -> SuccessorResult<RegistrationIntentV1, P, S> {
    let m = candidate.manifest();
    Ok(RegistrationIntentV1 {
        chain_id: m.chain_id,
        genesis_hash: m.genesis_hash,
        operation: AttestationOperationV1::TransitionEnclaveMeasurement,
        attestation_mode: p.attestation_mode,
        policy_hash: p.policy_hash().map_err(RenewalError::from)?,
        node_id: m.node_id.clone(),
        enclave_id: m.enclave_id().map_err(RenewalError::from)?,
        binding_id: binding,
        binding_version: b
            .binding_version
            .checked_add(1)
            .ok_or(RenewalError::CounterOverflow)?,
        registration_version: b
            .registration_version
            .checked_add(1)
            .ok_or(RenewalError::CounterOverflow)?,
        renewal_nonce: b.renewal_nonce,
        transition_nonce: b
            .transition_nonce
            .checked_add(1)
            .ok_or(RenewalError::CounterOverflow)?,
        requested_valid_until: valid_until,
        recipient_x25519: m.recipient_x25519,
        attestation_ed25519: m.attestation_ed25519,
        noise_responder_x25519: m.noise_responder_x25519,
        node_host_authorization_hash: b.node_host_authorization_hash,
    })
}

pub(crate) fn validate_lease<P: LifecycleChainPort, S: LifecycleSignerV1>(
    intent: &RegistrationIntentV1,
    p: &TeePolicyV1,
    now: u64,
) -> SuccessorResult<(), P, S> {
    let lease = intent
        .requested_valid_until
        .checked_sub(now)
        .ok_or(SuccessorError::LeaseWindow)?;
    if lease < p.minimum_lease || lease > p.maximum_lease {
        return Err(SuccessorError::LeaseWindow);
    }
    Ok(())
}

pub(crate) fn onboarding_context<P: LifecycleChainPort, S: LifecycleSignerV1>(
    i: &RegistrationIntentV1,
    v: &LifecycleRegistryStateV1,
) -> SuccessorResult<crate::dcap_protocol::DcapOnboardingContextV1, P, S> {
    Ok(crate::dcap_protocol::DcapOnboardingContextV1 {
        chain_id: i.chain_id,
        genesis_hash: i.genesis_hash,
        intent_hash: i.intent_hash().map_err(RenewalError::from)?,
        node_id_hash: i.node_id.node_id_hash().map_err(RenewalError::from)?,
        enclave_id: i.enclave_id,
        binding_id: i.binding_id,
        policy_hash: i.policy_hash,
        recipient_x25519: i.recipient_x25519,
        network_encryption_public: v.network_encryption_public_key.0,
        key_epoch: v.key_epoch,
        network_encryption_epoch: v.network_encryption_epoch,
    })
}

pub(crate) fn source_target(b: &RenewalBindingV1) -> ServiceBindingTargetV1 {
    ServiceBindingTargetV1 {
        node_id_hash: b.node_id_hash,
        enclave_id: b.enclave_id,
        binding_id: b.binding_id,
        binding_version: b.binding_version,
        registration_version: b.registration_version,
        renewal_nonce: b.renewal_nonce,
        transition_nonce: b.transition_nonce,
        valid_until: b.valid_until,
        policy_hash: b.policy_hash,
        recipient_x25519: b.recipient_x25519,
    }
}

pub(crate) fn registry_gas<P: LifecycleChainPort, S: LifecycleSignerV1>(
    kind: RegistryMutatorV1,
    call: &[u8],
    evidence: &[u8],
    p: &TeePolicyV1,
) -> SuccessorResult<u64, P, S> {
    TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            kind,
            call.len(),
            evidence.len(),
            p.measurement_rules.len(),
            p.attestation_mode,
        )
        .map_err(RenewalError::from)
        .map_err(Into::into)
}

pub(crate) async fn fresh_nonce<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    signer: &S,
    journal: &mut ServiceTxJournalV1,
    live: &InProcessTeeContext,
    expected_kind: ServiceTxKindV1,
) -> SuccessorResult<u64, P, S> {
    let previous = journal.load().map_err(RenewalError::from)?;
    match reconcile_service_tx_v1(port, journal, live)
        .await
        .map_err(RenewalError::from)?
    {
        ServiceTxReconciliationV1::WaitForFinality { .. }
        | ServiceTxReconciliationV1::ReplayExact
        | ServiceTxReconciliationV1::IngestOnboarding => Err(SuccessorError::PendingOperation),
        ServiceTxReconciliationV1::ReplaceLostIdentity { nonce } => {
            if previous.is_some_and(|r| r.kind != expected_kind) {
                Err(SuccessorError::PendingOperation)
            } else {
                Ok(nonce)
            }
        }
        ServiceTxReconciliationV1::RebuildAndResign { nonce } => Ok(nonce),
        _ => port
            .account_nonce(signer.address(), LifecycleNonceTagV1::Pending)
            .await
            .map_err(RenewalError::from)
            .map_err(Into::into),
    }
}

pub(crate) async fn send<P: LifecycleChainPort, S: LifecycleSignerV1>(
    port: &P,
    record: &ServiceTxRecordV1,
    replayed: bool,
) -> SuccessorResult<SuccessorStep, P, S> {
    Ok(match broadcast::<P, S>(port, record, replayed).await? {
        crate::lifecycle::renewal::RenewalStep::Submitted {
            transaction_hash,
            replayed,
        } => SuccessorStep::Submitted {
            transaction_hash,
            replayed,
        },
        _ => SuccessorStep::AwaitingFinality,
    })
}
