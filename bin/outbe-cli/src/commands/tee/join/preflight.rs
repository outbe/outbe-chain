use super::super::CliFinalityRpc;
use super::{
    committed_manifest_matches_binding, ensure_durable_join_registration_caller,
    ensure_joinable_binding, finalized_binding_matches_intent,
    persist_authorized_join_admission_anchor_v1, DurableJoinSubmissionV1,
};
use crate::errors::TeeJoinError;
use crate::rpc::Rpc;
use alloy_primitives::{Address, B256, U256};
use eyre::{Result, WrapErr};
use outbe_operator::tee::{
    read_finalized_registry_view_v1, read_finalized_staged_successor_policy_v1,
    FinalizedRegistryChainViewV1, NodeBindingSelectorV1, RenewalBindingV1,
};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, RegistrationIntentV1, TeePolicyV1,
};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::{
    connect_committed_node_host_enclave, load_committed_enclave_manifest_v1,
    load_committed_join_submission, load_replacement_candidate_submission,
};
use std::path::Path;

pub(super) async fn resolve_join_policy(
    client: &(impl Rpc + Sync),
    rpc_chain_id: u64,
    finalized: &FinalizedRegistryChainViewV1,
    successor_policy: bool,
) -> Result<TeePolicyV1> {
    let policy = if successor_policy {
        let staged = read_finalized_staged_successor_policy_v1(&CliFinalityRpc(client))
            .await?
            .ok_or(TeeJoinError::NoApprovedSuccessorPolicy)?;
        if staged.finalized_hash != finalized.view.block_hash {
            return Err(TeeJoinError::SuccessorPolicyHeadMismatch.into());
        }
        staged.policy
    } else {
        finalized.policy.clone()
    };
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        return Err(TeeJoinError::PolicyChainIdMismatch.into());
    }
    Ok(policy)
}

pub(super) async fn verify_finalized_node_ownership(
    client: &(impl Rpc + Sync),
    finalized: &FinalizedRegistryChainViewV1,
    node_id_hash: B256,
    signer_address: Address,
) -> Result<()> {
    if finalized
        .binding
        .as_ref()
        .is_some_and(|binding| binding.node_id_hash != node_id_hash)
    {
        return Err(TeeJoinError::SelectorNodeIdentityMismatch.into());
    }
    if let Some(node_binding) = finalized.binding.as_ref() {
        let signer_view = read_finalized_registry_view_v1(
            &CliFinalityRpc(client),
            &NodeBindingSelectorV1::Validator(signer_address),
        )
        .await
        .wrap_err("authenticate global EVM signer against finalized TeeRegistry association")?;
        if signer_view.binding.as_ref() != Some(node_binding) {
            return Err(TeeJoinError::SignerDoesNotOwnAssociation.into());
        }
    }
    Ok(())
}

pub(super) fn load_durable_checkpoint(
    node_data_dir: Option<&Path>,
    signer_address: Address,
) -> Result<(
    Option<DurableJoinSubmissionV1>,
    Option<RegistrationIntentV1>,
)> {
    let durable_submission = match node_data_dir {
        Some(dir) => load_node_dir_submission(dir)?,
        None => None,
    };
    ensure_durable_join_registration_caller(
        durable_submission
            .as_ref()
            .and_then(DurableJoinSubmissionV1::registration_caller),
        signer_address,
    )?;
    let durable_resume_intent = durable_submission
        .as_ref()
        .map(decode_submission_intent)
        .transpose()?;
    Ok((durable_submission, durable_resume_intent))
}

fn load_node_dir_submission(node_data_dir: &Path) -> Result<Option<DurableJoinSubmissionV1>> {
    let manifest_path = node_data_dir
        .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
        .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1);
    if !manifest_path.exists() {
        return Ok(None);
    }
    let candidate = load_replacement_candidate_submission(node_data_dir)
        .map_err(TeeJoinError::InspectCandidateCheckpoint)?;
    let committed = load_committed_join_submission(node_data_dir)
        .map_err(TeeJoinError::InspectCommittedCheckpoint)?;
    match (candidate, committed) {
        (Some(_), Some(_)) => Err(TeeJoinError::CoexistingCheckpoints.into()),
        (Some(value), None) => Ok(Some(DurableJoinSubmissionV1::Candidate(value))),
        (None, Some(value)) => Ok(Some(DurableJoinSubmissionV1::Committed(value))),
        (None, None) => Ok(None),
    }
}

fn decode_submission_intent(submission: &DurableJoinSubmissionV1) -> Result<RegistrationIntentV1> {
    let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence())
        .map_err(TeeJoinError::DecodeDurableCheckpoint)?;
    Ok(match evidence {
        AttestationEvidenceV1::Dcap(value) => value.intent,
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent,
    })
}

pub(super) enum PreflightOutcome {
    CompletedReplay,
    Proceed { resumes_finalized_target: bool },
}

pub(super) struct ReplayCheckInput<'a> {
    pub(super) enclave_socket: &'a str,
    pub(super) node_data_dir: Option<&'a Path>,
    pub(super) finalized: &'a FinalizedRegistryChainViewV1,
    pub(super) durable_resume_intent: Option<&'a RegistrationIntentV1>,
    pub(super) binding_id: B256,
    pub(super) valid_until: u64,
    pub(super) policy: &'a TeePolicyV1,
}

pub(super) fn evaluate_replay_and_lease(input: ReplayCheckInput<'_>) -> Result<PreflightOutcome> {
    let resumes_finalized_target = match (&input.finalized.binding, input.durable_resume_intent) {
        (Some(binding), Some(intent)) => finalized_binding_matches_intent(binding, intent)?,
        _ => false,
    };
    if resumes_finalized_target {
        return Ok(PreflightOutcome::Proceed {
            resumes_finalized_target: true,
        });
    }
    if let Some(binding) = input
        .finalized
        .binding
        .as_ref()
        .filter(|b| input.finalized.schedule.finalized_timestamp < b.valid_until)
    {
        if try_complete_exact_replay(&input, binding)? {
            return Ok(PreflightOutcome::CompletedReplay);
        }
        ensure_joinable_binding(Some(binding), input.finalized.schedule.finalized_timestamp)?;
    }
    let lease = input
        .valid_until
        .checked_sub(input.finalized.schedule.finalized_timestamp)
        .ok_or(TeeJoinError::ValidUntilBeforeFinalizedTime)?;
    if lease < input.policy.minimum_lease || lease > input.policy.maximum_lease {
        return Err(TeeJoinError::LeaseOutsidePolicyRange {
            lease,
            min: input.policy.minimum_lease,
            max: input.policy.maximum_lease,
        }
        .into());
    }
    Ok(PreflightOutcome::Proceed {
        resumes_finalized_target: false,
    })
}

fn try_complete_exact_replay(
    input: &ReplayCheckInput<'_>,
    binding: &RenewalBindingV1,
) -> Result<bool> {
    if binding.binding_id != input.binding_id || binding.valid_until != input.valid_until {
        return Ok(false);
    }
    let Some(node_data_dir) = input.node_data_dir else {
        return Ok(false);
    };
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(TeeJoinError::LoadReplayManifest)?;
    if !committed_manifest_matches_binding(&manifest, binding)? {
        return Ok(false);
    }
    let mut committed = connect_committed_node_host_enclave(input.enclave_socket, node_data_dir)
        .map_err(TeeJoinError::ReopenCompletedJoin)?;
    match committed.request(&EnclaveRequest::GetPublicKeys)? {
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            recipient_x25519_pub,
            ..
        } if recipient_x25519_pub
            == <B256 as Into<[u8; 32]>>::into(input.finalized.tribute_offer_public) =>
        {
            persist_authorized_join_admission_anchor_v1(
                node_data_dir,
                input.finalized,
                binding.node_id_hash,
                binding.enclave_id,
                binding.intent_hash,
            )?;
            println!("[ok] exact tee join is already finalized and locally committed");
            Ok(true)
        }
        other => Err(TeeJoinError::ReplayEnclaveNotReady(format!("{other:?}")).into()),
    }
}
