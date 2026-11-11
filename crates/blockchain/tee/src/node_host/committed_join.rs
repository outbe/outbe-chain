use super::codec_error;
use super::durable_submission::{
    verify_durable_submission, DurableSubmission, DurableSubmissionKind,
};
use super::filesystem::remove_file_and_sync_directory;
use super::journal_records::{persist_exact_checkpoint, CheckedRelayInput, ExactCheckpoint};
use super::locked_state::{lock_node_host_state, require_committed_node_host_state};
use super::path_exists;
use super::read_committed_join_relay;
use super::read_committed_join_submission;
use super::read_finalized_join_admission_anchor;
use super::read_manifest;
use super::read_replacement_candidate;
use super::read_replacement_submission;
use super::reconcile_committed_join_state;
use super::reconcile_finalized_join_admission_anchor;
use super::reconcile_replacement_state;
use super::replace_bytes_atomically;
use super::validate_durable_replacement_submission;
use super::CommittedJoinRelayV1;
use super::CommittedJoinSubmissionV1;
use super::FinalizedJoinAdmissionAnchorV1;
use super::NodeHostPaths;
use super::NodeHostStateLock;

use crate::NodeHostNoiseKey;
use crate::TransportError;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;

use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use std::path::Path;

// The lock remains owned by this value through each caller's reads and writes.
struct LockedJoinState {
    paths: NodeHostPaths,
    manifest: EnclaveInitializationManifestV1,
    _state_lock: NodeHostStateLock,
}

fn locked_join_state(
    node_data_dir: &Path,
    load_manifest: impl FnOnce(
        &NodeHostPaths,
    ) -> Result<EnclaveInitializationManifestV1, TransportError>,
) -> Result<LockedJoinState, TransportError> {
    let (paths, state_lock) = lock_node_host_state(node_data_dir)?;
    let manifest = load_manifest(&paths)?;
    Ok(LockedJoinState {
        paths,
        manifest,
        _state_lock: state_lock,
    })
}

fn committed_submission_state(
    node_data_dir: &Path,
    missing_state_error: &'static str,
) -> Result<LockedJoinState, TransportError> {
    locked_join_state(node_data_dir, |paths| {
        require_committed_node_host_state(paths, missing_state_error)?;
        let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
        let manifest = read_manifest(&paths.manifest)?;
        if manifest.node_host_noise_x25519 != node_host.public() {
            return Err(TransportError::Codec(
                "committed join manifest does not match the persistent NodeHost key".into(),
            ));
        }
        reconcile_committed_join_state(paths, &manifest)?;
        Ok(manifest)
    })
}

fn finalized_anchor_state(node_data_dir: &Path) -> Result<LockedJoinState, TransportError> {
    locked_join_state(node_data_dir, |paths| {
        let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
        reconcile_replacement_state(paths, &node_host)?;
        let manifest = read_manifest(&paths.manifest)?;
        reconcile_committed_join_state(paths, &manifest)?;
        reconcile_finalized_join_admission_anchor(paths)?;
        Ok(manifest)
    })
}

/// Persist exact canonical registration material for the already committed
/// enclave. Exact replay is idempotent. The function rejects conflicting
/// material.
pub fn persist_committed_join_submission(
    node_data_dir: &Path,
    registration_caller: Address,
    evidence: &AttestationEvidenceV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> Result<CommittedJoinSubmissionV1, TransportError> {
    let state = committed_submission_state(
        node_data_dir,
        "committed join submission requires committed NodeHost state",
    )?;
    let paths = &state.paths;
    let manifest = &state.manifest;
    let submission = CommittedJoinSubmissionV1::new(
        registration_caller,
        evidence.encode_canonical().map_err(codec_error)?,
        *node_signature,
        *enclave_signature,
    );
    validate_durable_committed_join_submission(manifest, &submission)?;
    let bytes = submission.encode_canonical()?;
    persist_exact_checkpoint(
        ExactCheckpoint {
            path: &paths.committed_join_submission,
            next: &paths.committed_join_submission_next,
            scratch: &paths.committed_join_write_scratch,
            root: &paths.root,
            read: read_committed_join_submission,
            conflict_error: "committed join material conflicts with the durable submission",
        },
        submission,
        &bytes,
    )
}

/// Reload and revalidate exact committed-enclave registration material.
pub fn load_committed_join_submission(
    node_data_dir: &Path,
) -> Result<Option<CommittedJoinSubmissionV1>, TransportError> {
    let state = committed_submission_state(
        node_data_dir,
        "committed join submission reload requires committed NodeHost state",
    )?;
    let paths = &state.paths;
    let manifest = &state.manifest;
    if !path_exists(&paths.committed_join_submission)? {
        return Ok(None);
    }
    let submission = read_committed_join_submission(&paths.committed_join_submission)?;
    validate_durable_committed_join_submission(manifest, &submission)?;
    Ok(Some(submission))
}

/// Persist the byte-identical signed committed-join transaction before its
/// first network send.
pub fn persist_committed_join_relay(
    node_data_dir: &Path,
    calldata_hash: B256,
    from_block: u64,
    raw_transaction: &[u8],
) -> Result<CommittedJoinRelayV1, TransportError> {
    let (paths, _state_lock) = lock_node_host_state(node_data_dir)?;
    let manifest = read_manifest(&paths.manifest)?;
    reconcile_committed_join_state(&paths, &manifest)?;
    if !path_exists(&paths.committed_join_submission)? {
        return Err(TransportError::Codec(
            "committed join relay requires durable submission state".into(),
        ));
    }
    let submission = read_committed_join_submission(&paths.committed_join_submission)?;
    validate_durable_committed_join_submission(&manifest, &submission)?;
    let checked = CheckedRelayInput::new(
        calldata_hash,
        raw_transaction,
        "committed join relay transaction is incomplete",
    )?;
    let submission_hash = submission.submission_hash()?;
    let material = checked.into_material();
    let relay = CommittedJoinRelayV1::new(submission_hash, from_block, material);
    let bytes = relay.encode_canonical()?;
    persist_exact_checkpoint(
        ExactCheckpoint {
            path: &paths.committed_join_relay,
            next: &paths.committed_join_relay_next,
            scratch: &paths.committed_join_write_scratch,
            root: &paths.root,
            read: read_committed_join_relay,
            conflict_error:
                "committed join transaction conflicts with the durable relay checkpoint",
        },
        relay,
        &bytes,
    )
}

/// Reload the byte-identical signed committed-join transaction after restart.
pub fn load_committed_join_relay(
    node_data_dir: &Path,
) -> Result<Option<CommittedJoinRelayV1>, TransportError> {
    let (paths, _state_lock) = lock_node_host_state(node_data_dir)?;
    let manifest = read_manifest(&paths.manifest)?;
    reconcile_committed_join_state(&paths, &manifest)?;
    if !path_exists(&paths.committed_join_relay)? {
        return Ok(None);
    }
    let submission = read_committed_join_submission(&paths.committed_join_submission)?;
    validate_durable_committed_join_submission(&manifest, &submission)?;
    let relay = read_committed_join_relay(&paths.committed_join_relay)?;
    if relay.submission_hash() != submission.submission_hash()? {
        return Err(TransportError::Codec(
            "committed join relay targets another durable submission".into(),
        ));
    }
    Ok(Some(relay))
}

/// Remove an exact committed-join checkpoint only after the caller has proved
/// the same intent completed locally. A crash between removals converges on
/// retry because the function removes relay before submission.
pub fn clear_committed_join_checkpoint(
    node_data_dir: &Path,
    expected_intent_hash: B256,
) -> Result<(), TransportError> {
    let (paths, _state_lock) = lock_node_host_state(node_data_dir)?;
    let manifest = read_manifest(&paths.manifest)?;
    reconcile_committed_join_state(&paths, &manifest)?;
    if !path_exists(&paths.committed_join_submission)? {
        return Ok(());
    }
    let submission = read_committed_join_submission(&paths.committed_join_submission)?;
    let intent = validate_durable_committed_join_submission(&manifest, &submission)?;
    if intent.intent_hash().map_err(codec_error)? != expected_intent_hash {
        return Err(TransportError::Codec(
            "committed join checkpoint belongs to another intent".into(),
        ));
    }
    remove_file_and_sync_directory(&paths.committed_join_relay, &paths.root)?;
    remove_file_and_sync_directory(&paths.committed_join_submission, &paths.root)?;
    Ok(())
}

/// Persist an exact finalized join checkpoint before any local promotion,
/// checkpoint cleanup, or successful CLI return. Exact replay is idempotent.
/// Only a strictly later checkpoint for the same chain and NodeHost identity
/// may replace it.
pub fn persist_finalized_join_admission_anchor(
    node_data_dir: &Path,
    anchor: FinalizedJoinAdmissionAnchorV1,
) -> Result<FinalizedJoinAdmissionAnchorV1, TransportError> {
    validate_finalized_join_admission_anchor(anchor)?;
    let state = finalized_anchor_state(node_data_dir)?;
    let paths = &state.paths;
    let manifest = &state.manifest;
    validate_anchor_against_local_state(paths, manifest, anchor)?;
    if let Some(pending_intent_hash) = durable_join_intent_hash(paths, manifest)? {
        if pending_intent_hash != anchor.intent_hash {
            return Err(TransportError::Codec(
                "finalized join admission anchor does not match the durable join intent".into(),
            ));
        }
    } else if has_incomplete_join_checkpoint(paths)? {
        return Err(TransportError::Codec(
            "unfinished join checkpoint is incomplete".into(),
        ));
    }

    if path_exists(&paths.finalized_join_admission_anchor)? {
        let durable = read_finalized_join_admission_anchor(&paths.finalized_join_admission_anchor)?;
        validate_anchor_replacement(durable, anchor)?;
        if durable == anchor {
            return Ok(durable);
        }
    }
    replace_bytes_atomically(
        &paths.finalized_join_admission_anchor,
        &paths.finalized_join_admission_anchor_next,
        &paths.finalized_join_admission_anchor_scratch,
        &anchor.encode_canonical(),
        &paths.root,
    )?;
    Ok(anchor)
}

/// Load the validator catch-up authority. An unfinished durable join without
/// its exact anchor fails closed so a stale prior anchor cannot be reused.
pub fn load_finalized_join_admission_anchor(
    node_data_dir: &Path,
) -> Result<Option<FinalizedJoinAdmissionAnchorV1>, TransportError> {
    let state = finalized_anchor_state(node_data_dir)?;
    let paths = &state.paths;
    let manifest = &state.manifest;
    let pending_intent_hash = durable_join_intent_hash(paths, manifest)?;
    if !path_exists(&paths.finalized_join_admission_anchor)? {
        if pending_intent_hash.is_some() || has_incomplete_join_checkpoint(paths)? {
            return Err(TransportError::Codec(
                "unfinished join has no finalized admission anchor".into(),
            ));
        }
        return Ok(None);
    }
    let anchor = read_finalized_join_admission_anchor(&paths.finalized_join_admission_anchor)?;
    validate_anchor_against_local_state(paths, manifest, anchor)?;
    if let Some(intent_hash) = pending_intent_hash {
        if intent_hash != anchor.intent_hash {
            return Err(TransportError::Codec(
                "unfinished join conflicts with the finalized admission anchor".into(),
            ));
        }
    } else if has_incomplete_join_checkpoint(paths)? {
        return Err(TransportError::Codec(
            "unfinished join checkpoint is incomplete".into(),
        ));
    }
    Ok(Some(anchor))
}

pub(super) fn validate_durable_committed_join_submission(
    manifest: &EnclaveInitializationManifestV1,
    submission: &CommittedJoinSubmissionV1,
) -> Result<RegistrationIntentV1, TransportError> {
    verify_durable_submission(
        manifest,
        DurableSubmission {
            evidence: submission.evidence(),
            node_signature: submission.node_signature(),
            enclave_signature: submission.enclave_signature(),
            kind: DurableSubmissionKind::CommittedJoin,
        },
    )
}

pub(super) fn validate_finalized_join_admission_anchor(
    anchor: FinalizedJoinAdmissionAnchorV1,
) -> Result<(), TransportError> {
    if anchor_identity_incomplete(anchor) || anchor_finality_incomplete(anchor) {
        return Err(TransportError::Codec(
            "finalized join admission anchor is incomplete".into(),
        ));
    }
    Ok(())
}

fn anchor_identity_incomplete(anchor: FinalizedJoinAdmissionAnchorV1) -> bool {
    anchor_network_identity_incomplete(anchor) || anchor_binding_identity_incomplete(anchor)
}

fn anchor_network_identity_incomplete(anchor: FinalizedJoinAdmissionAnchorV1) -> bool {
    anchor.chain_id == [0; 32] || anchor.genesis_hash.is_zero() || anchor.node_id_hash.is_zero()
}

fn anchor_binding_identity_incomplete(anchor: FinalizedJoinAdmissionAnchorV1) -> bool {
    anchor.enclave_id.is_zero() || anchor.intent_hash.is_zero()
}

fn anchor_finality_incomplete(anchor: FinalizedJoinAdmissionAnchorV1) -> bool {
    anchor.finalized_height == 0
        || anchor.finalized_hash.is_zero()
        || anchor.finalized_state_root.is_zero()
        || anchor.finalized_consensus_timestamp == 0
}

pub(super) fn validate_anchor_replacement(
    durable: FinalizedJoinAdmissionAnchorV1,
    requested: FinalizedJoinAdmissionAnchorV1,
) -> Result<(), TransportError> {
    if durable == requested {
        return Ok(());
    }
    if anchor_identity_changed(durable, requested) || anchor_not_newer(durable, requested) {
        return Err(TransportError::Codec(
            "finalized join admission anchor replacement must be newer for the same chain and NodeHost identity; requested value conflicts with durable state".into(),
        ));
    }
    Ok(())
}

fn anchor_identity_changed(
    durable: FinalizedJoinAdmissionAnchorV1,
    requested: FinalizedJoinAdmissionAnchorV1,
) -> bool {
    durable.chain_id != requested.chain_id
        || durable.genesis_hash != requested.genesis_hash
        || durable.node_id_hash != requested.node_id_hash
}

fn anchor_not_newer(
    durable: FinalizedJoinAdmissionAnchorV1,
    requested: FinalizedJoinAdmissionAnchorV1,
) -> bool {
    requested.finalized_height <= durable.finalized_height
        || requested.finalized_consensus_timestamp < durable.finalized_consensus_timestamp
}

fn validate_anchor_against_local_state(
    paths: &NodeHostPaths,
    active: &EnclaveInitializationManifestV1,
    anchor: FinalizedJoinAdmissionAnchorV1,
) -> Result<(), TransportError> {
    validate_finalized_join_admission_anchor(anchor)?;
    let active_node_id_hash = active.node_id.node_id_hash().map_err(codec_error)?;
    if anchor.chain_id != active.chain_id
        || anchor.genesis_hash != active.genesis_hash
        || anchor.node_id_hash != active_node_id_hash
    {
        return Err(TransportError::Codec(
            "finalized join admission anchor targets another chain or NodeHost identity".into(),
        ));
    }
    let active_enclave_id = active.enclave_id().map_err(codec_error)?;
    let candidate_enclave_id = if path_exists(&paths.replacement_candidate)? {
        Some(
            read_replacement_candidate(&paths.replacement_candidate)?
                .manifest
                .enclave_id()
                .map_err(codec_error)?,
        )
    } else {
        None
    };
    if anchor.enclave_id != active_enclave_id && candidate_enclave_id != Some(anchor.enclave_id) {
        return Err(TransportError::Codec(
            "finalized join admission anchor targets another local enclave".into(),
        ));
    }
    Ok(())
}

fn durable_join_intent_hash(
    paths: &NodeHostPaths,
    active: &EnclaveInitializationManifestV1,
) -> Result<Option<B256>, TransportError> {
    let candidate_submission = path_exists(&paths.replacement_submission)?;
    let committed_submission = path_exists(&paths.committed_join_submission)?;
    if candidate_submission && committed_submission {
        return Err(TransportError::Codec(
            "candidate and committed join checkpoints coexist".into(),
        ));
    }
    if candidate_submission {
        if !path_exists(&paths.replacement_candidate)? {
            return Err(TransportError::Codec(
                "replacement submission is missing its candidate".into(),
            ));
        }
        let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
        let submission = read_replacement_submission(&paths.replacement_submission)?;
        let intent = validate_durable_replacement_submission(&candidate.manifest, &submission)?;
        return intent.intent_hash().map(Some).map_err(codec_error);
    }
    if committed_submission {
        let submission = read_committed_join_submission(&paths.committed_join_submission)?;
        let intent = validate_durable_committed_join_submission(active, &submission)?;
        return intent.intent_hash().map(Some).map_err(codec_error);
    }
    Ok(None)
}

fn has_incomplete_join_checkpoint(paths: &NodeHostPaths) -> Result<bool, TransportError> {
    if has_incomplete_replacement_checkpoint(paths)? {
        return Ok(true);
    }
    has_incomplete_committed_checkpoint(paths)
}

// Each probe is fallible. Keep the replacement paths before the committed paths
// and stop as soon as an existing checkpoint is found.
fn has_incomplete_replacement_checkpoint(paths: &NodeHostPaths) -> Result<bool, TransportError> {
    if path_exists(&paths.replacement_candidate)? {
        return Ok(true);
    }
    if path_exists(&paths.replacement_submission)? {
        return Ok(true);
    }
    path_exists(&paths.replacement_relay)
}

fn has_incomplete_committed_checkpoint(paths: &NodeHostPaths) -> Result<bool, TransportError> {
    if path_exists(&paths.committed_join_submission)? {
        return Ok(true);
    }
    path_exists(&paths.committed_join_relay)
}
