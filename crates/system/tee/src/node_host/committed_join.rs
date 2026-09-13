use super::codec_error;
use super::ensure_private_directory;
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
use super::remove_file_if_exists;
use super::replace_bytes_atomically;
use super::validate_durable_replacement_submission;
use super::CommittedJoinRelayV1;
use super::CommittedJoinSubmissionV1;
use super::FinalizedJoinAdmissionAnchorV1;
use super::NodeHostPaths;
use super::NodeHostStateLock;

use crate::NodeHostNoiseKey;
use crate::TransportError;
use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;
use outbe_primitives::tee_attestation_v1::AttestationOperationV1;

use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use std::fs::File;

use std::path::Path;

/// Persist exact canonical registration material for the already committed
/// enclave. Exact replay is idempotent; conflicting material is rejected.
pub fn persist_committed_join_submission(
    node_data_dir: &Path,
    registration_caller: Address,
    evidence: &AttestationEvidenceV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> Result<CommittedJoinSubmissionV1, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "committed join submission requires committed NodeHost state".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    let manifest = read_manifest(&paths.manifest)?;
    if manifest.node_host_noise_x25519 != node_host.public() {
        return Err(TransportError::Codec(
            "committed join manifest does not match the persistent NodeHost key".into(),
        ));
    }
    reconcile_committed_join_state(&paths, &manifest)?;
    let submission = CommittedJoinSubmissionV1 {
        registration_caller,
        evidence: evidence.encode_canonical().map_err(codec_error)?,
        node_signature: *node_signature,
        enclave_signature: *enclave_signature,
    };
    validate_durable_committed_join_submission(&manifest, &submission)?;
    let bytes = submission.encode_canonical()?;
    if path_exists(&paths.committed_join_submission)? {
        let durable = read_committed_join_submission(&paths.committed_join_submission)?;
        if durable == submission {
            return Ok(durable);
        }
        return Err(TransportError::Codec(
            "committed join material conflicts with the durable submission".into(),
        ));
    }
    replace_bytes_atomically(
        &paths.committed_join_submission,
        &paths.committed_join_submission_next,
        &paths.committed_join_write_scratch,
        &bytes,
        &paths.root,
    )?;
    Ok(submission)
}

/// Reload and revalidate exact committed-enclave registration material.
pub fn load_committed_join_submission(
    node_data_dir: &Path,
) -> Result<Option<CommittedJoinSubmissionV1>, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "committed join submission reload requires committed NodeHost state".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    let manifest = read_manifest(&paths.manifest)?;
    if manifest.node_host_noise_x25519 != node_host.public() {
        return Err(TransportError::Codec(
            "committed join manifest does not match the persistent NodeHost key".into(),
        ));
    }
    reconcile_committed_join_state(&paths, &manifest)?;
    if !path_exists(&paths.committed_join_submission)? {
        return Ok(None);
    }
    let submission = read_committed_join_submission(&paths.committed_join_submission)?;
    validate_durable_committed_join_submission(&manifest, &submission)?;
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
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    let manifest = read_manifest(&paths.manifest)?;
    reconcile_committed_join_state(&paths, &manifest)?;
    if !path_exists(&paths.committed_join_submission)? {
        return Err(TransportError::Codec(
            "committed join relay requires durable submission state".into(),
        ));
    }
    let submission = read_committed_join_submission(&paths.committed_join_submission)?;
    validate_durable_committed_join_submission(&manifest, &submission)?;
    if calldata_hash.is_zero() || raw_transaction.is_empty() {
        return Err(TransportError::Codec(
            "committed join relay transaction is incomplete".into(),
        ));
    }
    let relay = CommittedJoinRelayV1 {
        submission_hash: submission.submission_hash()?,
        calldata_hash,
        transaction_hash: keccak256(raw_transaction),
        from_block,
        raw_transaction: raw_transaction.to_vec(),
    };
    let bytes = relay.encode_canonical()?;
    if path_exists(&paths.committed_join_relay)? {
        let durable = read_committed_join_relay(&paths.committed_join_relay)?;
        if durable == relay {
            return Ok(durable);
        }
        return Err(TransportError::Codec(
            "committed join transaction conflicts with the durable relay checkpoint".into(),
        ));
    }
    replace_bytes_atomically(
        &paths.committed_join_relay,
        &paths.committed_join_relay_next,
        &paths.committed_join_write_scratch,
        &bytes,
        &paths.root,
    )?;
    Ok(relay)
}

/// Reload the byte-identical signed committed-join transaction after restart.
pub fn load_committed_join_relay(
    node_data_dir: &Path,
) -> Result<Option<CommittedJoinRelayV1>, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    let manifest = read_manifest(&paths.manifest)?;
    reconcile_committed_join_state(&paths, &manifest)?;
    if !path_exists(&paths.committed_join_relay)? {
        return Ok(None);
    }
    let submission = read_committed_join_submission(&paths.committed_join_submission)?;
    validate_durable_committed_join_submission(&manifest, &submission)?;
    let relay = read_committed_join_relay(&paths.committed_join_relay)?;
    if relay.submission_hash != submission.submission_hash()? {
        return Err(TransportError::Codec(
            "committed join relay targets another durable submission".into(),
        ));
    }
    Ok(Some(relay))
}

/// Remove an exact committed-join checkpoint only after the caller has proved
/// the same intent completed locally. A crash between removals converges on
/// retry because relay is removed before submission.
pub fn clear_committed_join_checkpoint(
    node_data_dir: &Path,
    expected_intent_hash: B256,
) -> Result<(), TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
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
    remove_file_if_exists(&paths.committed_join_relay)?;
    File::open(&paths.root)?.sync_all()?;
    remove_file_if_exists(&paths.committed_join_submission)?;
    File::open(&paths.root)?.sync_all()?;
    Ok(())
}

/// Persist an exact finalized join checkpoint before any local promotion,
/// checkpoint cleanup, or successful CLI return. Exact replay is idempotent;
/// only a strictly later checkpoint for the same chain and NodeHost identity
/// may replace it.
pub fn persist_finalized_join_admission_anchor(
    node_data_dir: &Path,
    anchor: FinalizedJoinAdmissionAnchorV1,
) -> Result<FinalizedJoinAdmissionAnchorV1, TransportError> {
    validate_finalized_join_admission_anchor(anchor)?;
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    reconcile_committed_join_state(&paths, &manifest)?;
    reconcile_finalized_join_admission_anchor(&paths)?;
    validate_anchor_against_local_state(&paths, &manifest, anchor)?;
    if let Some(pending_intent_hash) = durable_join_intent_hash(&paths, &manifest)? {
        if pending_intent_hash != anchor.intent_hash {
            return Err(TransportError::Codec(
                "finalized join admission anchor does not match the durable join intent".into(),
            ));
        }
    } else if has_incomplete_join_checkpoint(&paths)? {
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
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    reconcile_committed_join_state(&paths, &manifest)?;
    reconcile_finalized_join_admission_anchor(&paths)?;
    let pending_intent_hash = durable_join_intent_hash(&paths, &manifest)?;
    if !path_exists(&paths.finalized_join_admission_anchor)? {
        if pending_intent_hash.is_some() || has_incomplete_join_checkpoint(&paths)? {
            return Err(TransportError::Codec(
                "unfinished join has no finalized admission anchor".into(),
            ));
        }
        return Ok(None);
    }
    let anchor = read_finalized_join_admission_anchor(&paths.finalized_join_admission_anchor)?;
    validate_anchor_against_local_state(&paths, &manifest, anchor)?;
    if let Some(intent_hash) = pending_intent_hash {
        if intent_hash != anchor.intent_hash {
            return Err(TransportError::Codec(
                "unfinished join conflicts with the finalized admission anchor".into(),
            ));
        }
    } else if has_incomplete_join_checkpoint(&paths)? {
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
    let evidence =
        AttestationEvidenceV1::decode_canonical(submission.evidence()).map_err(codec_error)?;
    let intent = match evidence {
        AttestationEvidenceV1::Dcap(value)
            if value.intent.operation == AttestationOperationV1::RegisterEnclave =>
        {
            value.intent
        }
        AttestationEvidenceV1::GramineDirectDev(value)
            if value.intent.operation == AttestationOperationV1::RegisterEnclave
                && value.dev_signature == *submission.enclave_signature() =>
        {
            value.intent
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::GramineDirectDev(_) => {
            return Err(TransportError::Codec(
                "committed join submission is not RegisterEnclave evidence".into(),
            ));
        }
    };
    manifest
        .validate_intent_binding(&intent)
        .map_err(codec_error)?;
    if !intent.verify_node_signature(submission.node_signature())
        || !intent.verify_enclave_signature(submission.enclave_signature())
    {
        return Err(TransportError::Codec(
            "committed join submission proof of possession is invalid".into(),
        ));
    }
    Ok(intent)
}

pub(super) fn validate_finalized_join_admission_anchor(
    anchor: FinalizedJoinAdmissionAnchorV1,
) -> Result<(), TransportError> {
    if anchor.chain_id == [0; 32]
        || anchor.genesis_hash.is_zero()
        || anchor.node_id_hash.is_zero()
        || anchor.enclave_id.is_zero()
        || anchor.intent_hash.is_zero()
        || anchor.finalized_height == 0
        || anchor.finalized_hash.is_zero()
        || anchor.finalized_state_root.is_zero()
        || anchor.finalized_consensus_timestamp == 0
    {
        return Err(TransportError::Codec(
            "finalized join admission anchor is incomplete".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_anchor_replacement(
    durable: FinalizedJoinAdmissionAnchorV1,
    requested: FinalizedJoinAdmissionAnchorV1,
) -> Result<(), TransportError> {
    if durable == requested {
        return Ok(());
    }
    if durable.chain_id != requested.chain_id
        || durable.genesis_hash != requested.genesis_hash
        || durable.node_id_hash != requested.node_id_hash
        || requested.finalized_height <= durable.finalized_height
        || requested.finalized_consensus_timestamp < durable.finalized_consensus_timestamp
    {
        return Err(TransportError::Codec(
            "finalized join admission anchor replacement must be newer for the same chain and NodeHost identity; requested value conflicts with durable state".into(),
        ));
    }
    Ok(())
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
    Ok(path_exists(&paths.replacement_candidate)?
        || path_exists(&paths.replacement_submission)?
        || path_exists(&paths.replacement_relay)?
        || path_exists(&paths.committed_join_submission)?
        || path_exists(&paths.committed_join_relay)?)
}
