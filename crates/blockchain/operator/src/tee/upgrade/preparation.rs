//! Private candidate key-ready preparation.
use super::submission::UpgradeSubmissionService;
use super::*;
use crate::tee::registry::{FinalizedRenewalChainViewV1, FinalizedStagedSuccessorPolicyV1};

struct PreparationViews {
    active: FinalizedRenewalChainViewV1,
    successor: FinalizedStagedSuccessorPolicyV1,
}

pub(super) async fn prepare_candidate_key_ready_v1<R: RenewalRpc + Sync, N: UpgradeNodeSignerV1>(
    service: &mut UpgradeSubmissionService<'_, R, N>,
) -> Result<()> {
    let views = load_views(service).await?;
    if recover_saved_candidate(service.node_data_dir, &views)? {
        return Ok(());
    }
    let prepared = build_candidate(service, &views)?;
    persist_key_ready(service, &views, prepared)
}

async fn load_views<R: RenewalRpc + Sync, N>(
    service: &UpgradeSubmissionService<'_, R, N>,
) -> Result<PreparationViews> {
    let rpc = service.rpc;
    let candidate = &*service.candidate;
    let node_data_dir = service.node_data_dir;
    let selector = service.selector;
    let checkpoint = inspect_upgrade_journal_v1(node_data_dir)?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    let UpgradeJournalStateV1::KeyProvisioned { ref context, .. } = checkpoint.lifecycle else {
        eyre::bail!("candidate key readiness requires the key-provisioned checkpoint");
    };
    let candidate_manifest_hash = candidate
        .manifest()
        .authorization_hash()
        .map_err(|error| eyre::eyre!("hash candidate manifest: {error}"))?;
    if candidate_manifest_hash != context.candidate_manifest_hash {
        eyre::bail!("connected candidate differs from the journaled candidate manifest");
    }
    let active = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    let successor = read_finalized_upgrade_policy_v1(rpc)
        .await?
        .ok_or_else(|| eyre::eyre!("no successor TEE policy is staged at finalized state"))?;
    if active.schedule.finalized_height != successor.finalized_height
        || active.schedule.finalized_hash != successor.finalized_hash
    {
        eyre::bail!("finalized active binding and staged policy were read at different heads");
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
        .map_err(|error| eyre::eyre!("hash active finalized policy: {error}"))?;
    if !successor_identity_matches(active, successor, context, active_policy_hash)? {
        eyre::bail!("staged successor is not the direct successor of the finalized active policy");
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
            .map_err(|e| eyre::eyre!("invalid successor: {e}"))?
            != active_policy_hash
    {
        return Ok(false);
    }
    if successor
        .policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("hash staged successor policy: {error}"))?
        != context.successor_policy_hash
    {
        return Ok(false);
    }
    Ok(successor.policy.activation_height == context.activation_height)
}

fn recover_saved_candidate(node_data_dir: &Path, views: &PreparationViews) -> Result<bool> {
    let PreparationViews { active, successor } = views;
    if let Some(durable) = load_replacement_candidate_submission(node_data_dir)
        .map_err(|error| eyre::eyre!("reload durable candidate submission: {error}"))?
    {
        let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
            .map_err(|e| eyre::eyre!("invalid durable transition: {e}"))?;
        if active.schedule.finalized_timestamp >= evidence.intent().requested_valid_until {
            ensure_transition_source_or_target_v1(&active.binding, evidence.intent())?;
            if transition_target_matches_v1(&active.binding, evidence.intent()) {
                eyre::bail!("expired transition already executed; finalize it before renewing");
            }
            outbe_tee::node_host::clear_expired_transition_submission_v1(
                node_data_dir,
                evidence
                    .intent()
                    .intent_hash()
                    .map_err(|e| eyre::eyre!("invalid intent: {e}"))?,
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
        .ok_or_else(|| eyre::eyre!("transition collateral margin underflows"))?;
    if prepared.intent.requested_valid_until > ceiling {
        let minimum = active
            .schedule
            .finalized_timestamp
            .checked_add(successor.policy.minimum_lease)
            .ok_or_else(|| eyre::eyre!("minimum transition lease overflows"))?;
        if ceiling < minimum {
            eyre::bail!("fresh Intel collateral cannot satisfy the successor minimum lease");
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
        .map_err(|error| eyre::eyre!("hash transition intent: {error}"))?;
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
    .map_err(|error| eyre::eyre!("persist exact candidate submission: {error}"))?;
    let proof = evidence
        .transition_key_ready_proof()
        .ok_or_else(|| eyre::eyre!("candidate transition evidence has no key-ready proof"))?;
    record_candidate_key_ready_v1(
        node_data_dir,
        evidence.intent(),
        proof,
        active.tribute_offer_public.into(),
    )?;
    Ok(())
}
