use super::codec_error;
use super::durable_submission::{
    candidate_promotion_intent, verify_bound_possession, verify_durable_submission,
    DurableSubmission, DurableSubmissionKind, PossessionErrors,
};
use super::filesystem::remove_file_and_sync_directory;
use super::identity::read_committed_identity;
use super::journal_records::{persist_exact_checkpoint, CheckedRelayInput, ExactCheckpoint};
use super::locked_state::{lock_node_host_state, require_committed_node_host_state};
use super::path_exists;
use super::read_manifest;
use super::read_replacement_candidate;
use super::read_replacement_promotion;
use super::read_replacement_relay;
use super::read_replacement_submission;
use super::reconcile_replacement_state;
use super::replace_bytes_atomically;
use super::replacement_authorization;
use super::replacement_binding::validate_finalized_replacement_binding;
use super::write_bytes_once_or_exact;
use super::BoundedRecordBytes;
use super::FinalizedReplacementAuthorizationV1;
use super::FinalizedReplacementBindingV1;
use super::NodeHostPaths;
use super::NodeHostStateLock;
use super::ReplacementCandidateRecordV1;
use super::ReplacementCandidateRelayV1;
use super::ReplacementCandidateSubmissionV1;
use super::MAX_INITIALIZATION_MANIFEST_BYTES;

use crate::node_host::NodeHostIdentityKey;
use crate::TransportError;

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;
use outbe_primitives::tee_attestation_v1::AttestationOperationV1;
use outbe_primitives::tee_attestation_v1::DcapEvidenceV1;
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use std::fs;

use std::fs::File;

use std::path::Path;

// A replacement operation owns this lock for its entire state transition.
struct LockedReplacementState {
    paths: NodeHostPaths,
    node_host: NodeHostIdentityKey,
    _state_lock: NodeHostStateLock,
}

#[derive(Clone, Copy)]
enum ReplacementRequirement {
    ExistingState,
    SubmissionWrite,
    SubmissionRead,
    Authorization,
}

impl ReplacementRequirement {
    fn missing_error(self) -> Option<&'static str> {
        match self {
            Self::ExistingState => None,
            Self::SubmissionWrite => {
                Some("replacement submission requires committed and candidate NodeHost state")
            }
            Self::SubmissionRead => {
                Some("replacement submission reload requires committed NodeHost state")
            }
            Self::Authorization => {
                Some("replacement authorization requires committed NodeHost state")
            }
        }
    }
}

fn locked_replacement_state(
    node_data_dir: &Path,
    requirement: ReplacementRequirement,
) -> Result<LockedReplacementState, TransportError> {
    let (paths, state_lock) = lock_node_host_state(node_data_dir)?;
    if let Some(error) = requirement.missing_error() {
        require_committed_node_host_state(&paths, error)?;
    }
    let node_host = NodeHostIdentityKey::load(&paths.identity_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    Ok(LockedReplacementState {
        paths,
        node_host,
        _state_lock: state_lock,
    })
}

/// Persist exact canonical replacement transaction material. An exact retry is
/// idempotent. The function rejects any conflict, so restart never silently
/// changes the quote, collateral or proof-of-possession signatures.
pub fn persist_replacement_candidate_submission(
    node_data_dir: &Path,
    evidence: &AttestationEvidenceV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> Result<ReplacementCandidateSubmissionV1, TransportError> {
    let state = locked_replacement_state(node_data_dir, ReplacementRequirement::SubmissionWrite)?;
    let paths = &state.paths;
    let node_host = &state.node_host;
    if !path_exists(&paths.replacement_candidate)? {
        return Err(TransportError::Codec(
            "replacement submission requires a durable candidate".into(),
        ));
    }
    let active = read_manifest(&paths.manifest)?;
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    validate_replacement_candidate_state(&candidate, &active, node_host)?;
    let submission = validated_replacement_submission(
        &candidate.manifest,
        evidence,
        node_signature,
        enclave_signature,
    )?;
    let bytes = submission.encode_canonical()?;
    persist_exact_checkpoint(
        ExactCheckpoint {
            path: &paths.replacement_submission,
            next: &paths.replacement_submission_next,
            scratch: &paths.replacement_write_scratch,
            root: &paths.root,
            read: read_replacement_submission,
            conflict_error:
                "replacement material conflicts with the durable replacement submission",
        },
        submission,
        &bytes,
    )
}

fn validated_replacement_submission(
    manifest: &EnclaveInitializationManifestV1,
    evidence: &AttestationEvidenceV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> Result<ReplacementCandidateSubmissionV1, TransportError> {
    let evidence_bytes = evidence.encode_canonical().map_err(codec_error)?;
    let intent = candidate_promotion_intent(
        manifest,
        evidence,
        enclave_signature,
        "candidate submission is not an allowed registration or successor operation",
    )?;
    verify_bound_possession(
        manifest,
        intent,
        node_signature,
        enclave_signature,
        PossessionErrors {
            node_signature: "replacement submission node signature is invalid",
            enclave_signature: "replacement submission enclave signature is invalid",
        },
    )?;
    Ok(ReplacementCandidateSubmissionV1::new(
        evidence_bytes,
        *node_signature,
        *enclave_signature,
    ))
}

/// Reload exact durable replacement transaction material after a relay or
/// NodeHost restart. Journal reconciliation completes only already-fsynced
/// candidate/submission writes and never promotes the active enclave.
pub fn load_replacement_candidate_submission(
    node_data_dir: &Path,
) -> Result<Option<ReplacementCandidateSubmissionV1>, TransportError> {
    let state = locked_replacement_state(node_data_dir, ReplacementRequirement::SubmissionRead)?;
    let paths = &state.paths;
    if !path_exists(&paths.replacement_submission)? {
        return Ok(None);
    }
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    validate_durable_replacement_submission(&candidate.manifest, &submission)?;
    Ok(Some(submission))
}

/// Discard an expired, unexecuted transition before preparing fresh evidence.
/// The caller must verify the finalized binding still points to the source.
#[doc(hidden)]
pub fn clear_expired_transition_submission_v1(
    node_data_dir: &Path,
    expected_intent: B256,
    finalized_timestamp: u64,
) -> Result<(), TransportError> {
    let state = locked_replacement_state(node_data_dir, ReplacementRequirement::ExistingState)?;
    let paths = &state.paths;
    if !path_exists(&paths.replacement_submission)? {
        return Ok(());
    }
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    let intent = validate_durable_replacement_submission(&candidate.manifest, &submission)?;
    if intent.operation != AttestationOperationV1::TransitionEnclaveMeasurement
        || intent.intent_hash().map_err(codec_error)? != expected_intent
        || intent.requested_valid_until > finalized_timestamp
    {
        return Err(TransportError::Codec(
            "transition is not the exact expired submission".into(),
        ));
    }
    remove_file_and_sync_directory(&paths.replacement_relay, &paths.root)?;
    remove_file_and_sync_directory(&paths.replacement_submission, &paths.root)?;
    Ok(())
}

/// Persist the exact signed transaction before the first relay attempt. The
/// transaction is inseparable from the already durable candidate submission.
pub fn persist_replacement_candidate_relay(
    node_data_dir: &Path,
    calldata_hash: B256,
    raw_transaction: &[u8],
) -> Result<ReplacementCandidateRelayV1, TransportError> {
    let state = locked_replacement_state(node_data_dir, ReplacementRequirement::ExistingState)?;
    let paths = &state.paths;
    if !path_exists(&paths.replacement_candidate)? || !path_exists(&paths.replacement_submission)? {
        return Err(TransportError::Codec(
            "replacement relay requires durable candidate submission state".into(),
        ));
    }
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    validate_durable_replacement_submission(&candidate.manifest, &submission)?;
    let checked = CheckedRelayInput::new(
        calldata_hash,
        raw_transaction,
        "replacement relay transaction is incomplete",
    )?;
    let submission_hash = submission.submission_hash()?;
    let material = checked.into_material();
    let relay = ReplacementCandidateRelayV1::new(submission_hash, material);
    let bytes = relay.encode_canonical()?;
    persist_exact_checkpoint(
        ExactCheckpoint {
            path: &paths.replacement_relay,
            next: &paths.replacement_relay_next,
            scratch: &paths.replacement_write_scratch,
            root: &paths.root,
            read: read_replacement_relay,
            conflict_error: "replacement transaction conflicts with the durable relay checkpoint",
        },
        relay,
        &bytes,
    )
}

/// Reload the byte-identical signed candidate transaction after restart.
pub fn load_replacement_candidate_relay(
    node_data_dir: &Path,
) -> Result<Option<ReplacementCandidateRelayV1>, TransportError> {
    let state = locked_replacement_state(node_data_dir, ReplacementRequirement::ExistingState)?;
    let paths = &state.paths;
    if !path_exists(&paths.replacement_relay)? {
        return Ok(None);
    }
    Ok(Some(read_bound_replacement_relay(paths)?))
}

pub(super) fn read_bound_replacement_relay(
    paths: &NodeHostPaths,
) -> Result<ReplacementCandidateRelayV1, TransportError> {
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    let relay = read_replacement_relay(&paths.replacement_relay)?;
    if relay.submission_hash() != submission.submission_hash()? {
        return Err(TransportError::Codec(
            "replacement relay targets another durable submission".into(),
        ));
    }
    Ok(relay)
}

/// Constructs promotion authority only when one exact consensus-finalized
/// Registry binding matches the locally durable replacement transaction.
///
/// The caller must obtain `finalized` through the node-local finalized-state
/// adapter. This function deliberately accepts neither RPC receipts nor an
/// operator override and returns only the opaque capability consumed by
/// [`promote_replacement_candidate`]. This low-level cross-crate seam does not
/// itself prove that a directly constructed binding is finalized.
#[doc(hidden)]
pub fn construct_finalized_replacement_authorization_v1(
    node_data_dir: &Path,
    finalized: &FinalizedReplacementBindingV1,
) -> Result<FinalizedReplacementAuthorizationV1, TransportError> {
    let state = locked_replacement_state(node_data_dir, ReplacementRequirement::Authorization)?;
    let paths = &state.paths;
    let node_host = &state.node_host;
    if !path_exists(&paths.replacement_candidate)? || !path_exists(&paths.replacement_submission)? {
        return Err(TransportError::Codec(
            "replacement authorization requires a complete durable candidate and submission".into(),
        ));
    }

    let active = read_manifest(&paths.manifest)?;
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    validate_replacement_candidate_state(&candidate, &active, node_host)?;
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    let intent = validate_durable_replacement_submission(&candidate.manifest, &submission)?;
    validate_finalized_replacement_binding(&intent, finalized)?;
    replacement_authorization(&candidate, &submission)
}

/// Atomically make the finalized replacement manifest the normal startup
/// target. The opaque capability prevents local receipt, RPC or operator flags
/// from substituting for the I6 finalized-state verifier.
pub fn promote_replacement_candidate(
    node_data_dir: &Path,
    authorization: &FinalizedReplacementAuthorizationV1,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let (paths, _state_lock) = lock_node_host_state(node_data_dir)?;
    require_committed_node_host_state(
        &paths,
        "replacement promotion requires committed NodeHost state",
    )?;
    let (active, node_host) = read_committed_identity(&paths)?;
    let candidate_exists = path_exists(&paths.replacement_candidate)?;
    let submission_exists = path_exists(&paths.replacement_submission)?;
    if !candidate_exists && !submission_exists {
        return resolve_completed_promotion(&paths, active, authorization);
    }
    if candidate_exists != submission_exists {
        return Err(TransportError::Codec(
            "replacement candidate and submission durability state is incomplete".into(),
        ));
    }

    let candidate = validate_staged_promotion(&paths, &active, &node_host, authorization)?;
    finish_promotion(&paths, candidate, authorization)
}

fn resolve_completed_promotion(
    paths: &NodeHostPaths,
    active: EnclaveInitializationManifestV1,
    authorization: &FinalizedReplacementAuthorizationV1,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let active_hash = active.authorization_hash().map_err(codec_error)?;
    if path_exists(&paths.replacement_promotion)?
        && active_hash == authorization.candidate_manifest_hash
        && read_replacement_promotion(&paths.replacement_promotion)? == *authorization
    {
        return Ok(active);
    }
    if active_hash == authorization.candidate_manifest_hash {
        return Err(TransportError::Codec(
            "completed promotion authorization does not match its durable receipt".into(),
        ));
    }
    Err(TransportError::Codec(
        "no replacement candidate is staged for this finalized authorization".into(),
    ))
}

fn validate_staged_promotion(
    paths: &NodeHostPaths,
    active: &EnclaveInitializationManifestV1,
    node_host: &NodeHostIdentityKey,
    authorization: &FinalizedReplacementAuthorizationV1,
) -> Result<ReplacementCandidateRecordV1, TransportError> {
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    validate_replacement_candidate_state(&candidate, active, node_host)?;
    let candidate_manifest_hash = candidate
        .manifest
        .authorization_hash()
        .map_err(codec_error)?;
    if candidate_manifest_hash != authorization.candidate_manifest_hash {
        return Err(TransportError::Codec(
            "finalized authorization targets another candidate manifest".into(),
        ));
    }
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    let intent = validate_durable_replacement_submission(&candidate.manifest, &submission)?;
    if intent.intent_hash().map_err(codec_error)? != authorization.intent_hash {
        return Err(TransportError::Codec(
            "finalized authorization targets another replacement intent".into(),
        ));
    }
    Ok(candidate)
}

fn finish_promotion(
    paths: &NodeHostPaths,
    candidate: ReplacementCandidateRecordV1,
    authorization: &FinalizedReplacementAuthorizationV1,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    replace_bytes_atomically(
        &paths.replacement_promotion,
        &paths.replacement_promotion_next,
        &paths.replacement_write_scratch,
        &authorization.encode_canonical(),
        &paths.root,
    )?;
    let manifest_bytes = candidate.manifest.encode_canonical().map_err(codec_error)?;
    write_bytes_once_or_exact(
        &paths.next_manifest,
        &paths.replacement_write_scratch,
        BoundedRecordBytes {
            bytes: &manifest_bytes,
            maximum_len: MAX_INITIALIZATION_MANIFEST_BYTES,
            label: "next replacement manifest",
        },
        &paths.root,
    )?;
    fs::rename(&paths.next_manifest, &paths.manifest)?;
    File::open(&paths.root)?.sync_all()?;
    remove_file_and_sync_directory(&paths.replacement_relay, &paths.root)?;
    remove_file_and_sync_directory(&paths.replacement_submission, &paths.root)?;
    remove_file_and_sync_directory(&paths.replacement_candidate, &paths.root)?;
    Ok(candidate.manifest)
}

pub(super) fn validate_replacement_candidate_state(
    record: &ReplacementCandidateRecordV1,
    active: &EnclaveInitializationManifestV1,
    node_host: &NodeHostIdentityKey,
) -> Result<(), TransportError> {
    if replacement_node_host_key_changed(record, active, node_host)
        || replacement_chain_identity_changed(record, active)
    {
        return Err(TransportError::Codec(
            "replacement candidate does not preserve committed NodeHost identity".into(),
        ));
    }
    if record.predecessor_manifest_hash != active.authorization_hash().map_err(codec_error)? {
        return Err(TransportError::Codec(
            "replacement candidate predecessor is not the committed manifest".into(),
        ));
    }
    if record.manifest.enclave_id().map_err(codec_error)?
        == active.enclave_id().map_err(codec_error)?
    {
        return Err(TransportError::Codec(
            "replacement candidate must have a fresh enclave identity".into(),
        ));
    }
    if record
        .manifest
        .node_host_authorization_hash()
        .map_err(codec_error)?
        != active.node_host_authorization_hash().map_err(codec_error)?
    {
        return Err(TransportError::Codec(
            "replacement candidate changed the persistent NodeHost authority".into(),
        ));
    }
    Ok(())
}

fn replacement_node_host_key_changed(
    record: &ReplacementCandidateRecordV1,
    active: &EnclaveInitializationManifestV1,
    node_host: &NodeHostIdentityKey,
) -> bool {
    active.node_host_noise_x25519 != node_host.public()
        || record.manifest.node_host_noise_x25519 != node_host.public()
}

fn replacement_chain_identity_changed(
    record: &ReplacementCandidateRecordV1,
    active: &EnclaveInitializationManifestV1,
) -> bool {
    record.manifest.chain_id != active.chain_id
        || record.manifest.genesis_hash != active.genesis_hash
        || record.manifest.node_id != active.node_id
}

pub(super) fn validate_durable_replacement_submission(
    manifest: &EnclaveInitializationManifestV1,
    submission: &ReplacementCandidateSubmissionV1,
) -> Result<RegistrationIntentV1, TransportError> {
    verify_durable_submission(
        manifest,
        DurableSubmission {
            evidence: submission.evidence(),
            node_signature: submission.node_signature(),
            enclave_signature: submission.enclave_signature(),
            kind: DurableSubmissionKind::Replacement,
        },
    )
}

pub(super) fn validate_candidate_key_ready_proof(
    manifest: &EnclaveInitializationManifestV1,
    evidence: &DcapEvidenceV1,
) -> Result<(), TransportError> {
    if evidence.intent.operation != AttestationOperationV1::TransitionEnclaveMeasurement {
        return Ok(());
    }
    let proof = evidence
        .transition_key_ready_proof
        .as_ref()
        .ok_or(crate::errors::NodeHostRecordError::TransitionEvidenceMissingKeyReadyProof)?;
    let expected_manifest_hash = manifest.authorization_hash().map_err(codec_error)?;
    if proof.candidate_manifest_hash != expected_manifest_hash {
        return Err(TransportError::Codec(
            "transition key-ready proof targets another durable candidate manifest".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_direct_dev_transition_proof(
    manifest: &EnclaveInitializationManifestV1,
    evidence: &outbe_primitives::tee_attestation_v1::NoAttestationDevEvidenceV1,
) -> Result<(), TransportError> {
    let proof = evidence
        .transition_key_ready_proof
        .as_ref()
        .ok_or(crate::errors::NodeHostRecordError::TransitionProofMissing)?;
    proof
        .verify_for_transition(&evidence.intent, proof.resident_offer_public)
        .map_err(codec_error)?;
    if proof.candidate_manifest_hash != manifest.authorization_hash().map_err(codec_error)? {
        return Err(TransportError::Codec(
            "transition proof targets another candidate".into(),
        ));
    }
    Ok(())
}

pub(super) fn is_candidate_promotion_operation(operation: AttestationOperationV1) -> bool {
    matches!(
        operation,
        AttestationOperationV1::RegisterEnclave
            | AttestationOperationV1::ReplaceEnclaveBinding
            | AttestationOperationV1::TransitionEnclaveMeasurement
    )
}
