//! Private candidate key-ready preparation.
use super::submission::UpgradeSubmissionService;
use super::*;
use crate::tee::registry::{FinalizedRenewalChainViewV1, FinalizedStagedSuccessorPolicyV1};

struct PreparationViews {
    active: FinalizedRenewalChainViewV1,
    successor: FinalizedStagedSuccessorPolicyV1,
}

pub(super) async fn prepare_candidate_key_ready_v1<
    R: RegistryRpc + Sync,
    N: UpgradeNodeSignerV1,
>(
    service: &mut UpgradeSubmissionService<'_, R, N>,
) -> Result<()> {
    let views = load_views(service).await?;
    if recover_saved_candidate(service.node_data_dir, &views)? {
        return Ok(());
    }
    let prepared = build_candidate(service, &views)?;
    persist_key_ready(service, &views, prepared)
}

async fn load_views<R: RegistryRpc + Sync, N>(
    service: &UpgradeSubmissionService<'_, R, N>,
) -> Result<PreparationViews> {
    let rpc = service.rpc;
    let candidate = &*service.candidate;
    let node_data_dir = service.node_data_dir;
    let selector = service.selector;
    let checkpoint =
        inspect_upgrade_journal_v1(node_data_dir)?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::KeyProvisioned { ref context, .. } = checkpoint.lifecycle else {
        return Err(UpgradeError::KeyReadyRequiresKeyProvisioned.into());
    };
    let candidate_manifest_hash = candidate
        .manifest()
        .authorization_hash()
        .map_err(UpgradeError::HashCandidateManifest)?;
    if candidate_manifest_hash != context.candidate_manifest_hash {
        return Err(UpgradeError::ConnectedCandidateManifestMismatch.into());
    }
    let active = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    let successor = read_finalized_upgrade_policy_v1(rpc)
        .await?
        .ok_or(UpgradeError::NoStagedSuccessorPolicy)?;
    if active.schedule.finalized_height != successor.finalized_height
        || active.schedule.finalized_hash != successor.finalized_hash
    {
        return Err(UpgradeError::FinalizedHeadMismatch.into());
    }
    validate_successor(&active, &successor, context)?;
    validate_candidate_identity_v1(candidate, selector, &active)?;

    Ok(PreparationViews { active, successor })
}

fn validate_successor(
    active: &FinalizedRenewalChainViewV1,
    successor: &FinalizedStagedSuccessorPolicyV1,
    context: &UpgradeContextV1,
) -> Result<()> {
    let active_policy_hash = active
        .policy
        .policy_hash()
        .map_err(UpgradeError::HashActiveFinalizedPolicy)?;
    if !successor_identity_matches(active, successor, context, active_policy_hash)? {
        return Err(UpgradeError::InvalidDirectSuccessorPolicy.into());
    }
    Ok(())
}

fn successor_identity_matches(
    active: &FinalizedRenewalChainViewV1,
    successor: &FinalizedStagedSuccessorPolicyV1,
    context: &UpgradeContextV1,
    active_policy_hash: B256,
) -> Result<bool> {
    if successor.policy.chain_id != active.policy.chain_id
        || successor.policy.genesis_hash != active.policy.genesis_hash
    {
        return Ok(false);
    }
    if successor.policy.predecessor_policy_hash != active_policy_hash
        && successor
            .policy
            .policy_hash()
            .map_err(UpgradeError::InvalidSuccessorPolicy)?
            != active_policy_hash
    {
        return Ok(false);
    }
    if successor
        .policy
        .policy_hash()
        .map_err(UpgradeError::HashStagedSuccessorPolicy)?
        != context.successor_policy_hash
    {
        return Ok(false);
    }
    Ok(successor.policy.activation_height == context.activation_height)
}

fn recover_saved_candidate(node_data_dir: &Path, views: &PreparationViews) -> Result<bool> {
    let PreparationViews { active, successor } = views;
    if let Some(durable) = load_replacement_candidate_submission(node_data_dir)
        .map_err(UpgradeError::ReloadDurableCandidateSubmission)?
    {
        let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
            .map_err(UpgradeError::InvalidDurableTransition)?;
        if active.schedule.finalized_timestamp >= evidence.intent().requested_valid_until {
            ensure_transition_source_or_target_v1(&active.binding, evidence.intent())?;
            if transition_target_matches_v1(&active.binding, evidence.intent())? {
                return Err(UpgradeError::ExpiredTransitionAlreadyExecuted.into());
            }
            outbe_tee::node_host::clear_expired_transition_submission_v1(
                node_data_dir,
                evidence
                    .intent()
                    .intent_hash()
                    .map_err(UpgradeError::InvalidTransitionIntent)?,
                active.schedule.finalized_timestamp,
            )?;
        } else {
            recover_candidate_key_ready_v1(
                node_data_dir,
                &durable,
                &successor.policy,
                active.tribute_offer_public,
            )?;
            return Ok(true);
        }
    }

    Ok(false)
}

fn build_candidate<R, N>(
    service: &mut UpgradeSubmissionService<'_, R, N>,
    views: &PreparationViews,
) -> Result<PreparedTransitionEvidenceV1> {
    let PreparationViews { active, successor } = views;
    let candidate = &mut *service.candidate;
    let binding_id = service.binding_id;
    let requested_valid_until = service.requested_valid_until;
    let desired = transition_intent_v1(
        candidate,
        active,
        &successor.policy,
        binding_id,
        requested_valid_until,
    )?;
    let mut prepared = generate_transition_evidence_v1(candidate, desired, &successor.policy)?;
    let ceiling = prepared
        .collateral_expiration
        .checked_sub(successor.policy.collateral_margin)
        .ok_or(UpgradeError::TransitionCollateralMarginUnderflow)?;
    if prepared.intent.requested_valid_until > ceiling {
        let minimum = active
            .schedule
            .finalized_timestamp
            .checked_add(successor.policy.minimum_lease)
            .ok_or(UpgradeError::MinimumTransitionLeaseOverflow)?;
        if ceiling < minimum {
            return Err(UpgradeError::CollateralBelowMinimumLease.into());
        }
        prepared.intent.requested_valid_until = ceiling;
        prepared = generate_transition_evidence_v1(candidate, prepared.intent, &successor.policy)?;
    }
    validate_transition_time_window_v1(
        &prepared,
        &successor.policy,
        active.schedule.finalized_timestamp,
    )?;
    Ok(prepared)
}

fn persist_key_ready<R, N: UpgradeNodeSignerV1>(
    service: &UpgradeSubmissionService<'_, R, N>,
    views: &PreparationViews,
    prepared: PreparedTransitionEvidenceV1,
) -> Result<()> {
    let active = &views.active;
    let node_signer = service.node_signer;
    let node_data_dir = service.node_data_dir;
    let intent_hash = prepared
        .intent
        .intent_hash()
        .map_err(UpgradeError::HashTransitionIntent)?;
    let node_signature = node_signer
        .sign_node_hash(intent_hash)
        .wrap_err("sign transition intent with node authority")?;
    let evidence = prepared.evidence;
    persist_replacement_candidate_submission(
        node_data_dir,
        &evidence,
        &node_signature,
        &prepared.enclave_signature,
    )
    .map_err(UpgradeError::PersistCandidateSubmission)?;
    let proof = evidence
        .transition_key_ready_proof()
        .ok_or(UpgradeError::MissingCandidateKeyReadyProof)?;
    record_candidate_key_ready_v1(
        node_data_dir,
        evidence.intent(),
        proof,
        active.tribute_offer_public.into(),
    )?;
    Ok(())
}

fn validate_transition_time_window_v1(
    prepared: &PreparedTransitionEvidenceV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    finalized_timestamp: u64,
) -> Result<()> {
    let lease = prepared
        .intent
        .requested_valid_until
        .checked_sub(finalized_timestamp)
        .ok_or(UpgradeError::TransitionLeaseAlreadyExpired)?;
    let ceiling = prepared
        .collateral_expiration
        .checked_sub(policy.collateral_margin)
        .ok_or(UpgradeError::TransitionCollateralMarginUnderflow)?;
    let lease_matches = prepared.collateral_issue_floor <= finalized_timestamp
        && lease >= policy.minimum_lease
        && lease <= policy.maximum_lease;
    if !lease_matches || prepared.intent.requested_valid_until > ceiling {
        return Err(UpgradeError::EvidenceViolatesLeaseWindow.into());
    }
    Ok(())
}

fn recover_candidate_key_ready_v1(
    node_data_dir: &Path,
    durable: &ReplacementCandidateSubmissionV1,
    successor: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    expected_offer_public: B256,
) -> Result<()> {
    let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .map_err(UpgradeError::DecodeDurableTransitionEvidence)?;
    if evidence.intent().operation != AttestationOperationV1::TransitionEnclaveMeasurement
        || evidence.intent().policy_hash
            != successor
                .policy_hash()
                .map_err(UpgradeError::HashStagedSuccessorPolicy)?
    {
        return Err(UpgradeError::DurableSubmissionPolicyMismatch.into());
    }
    let proof = evidence
        .transition_key_ready_proof()
        .ok_or(UpgradeError::MissingDurableKeyReadyProof)?;
    record_candidate_key_ready_v1(
        node_data_dir,
        evidence.intent(),
        proof,
        expected_offer_public.into(),
    )?;
    Ok(())
}
