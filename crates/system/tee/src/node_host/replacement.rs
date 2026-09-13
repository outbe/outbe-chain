use super::codec_error;
use super::ensure_private_directory;
use super::path_exists;
use super::read_manifest;
use super::read_replacement_candidate;
use super::read_replacement_promotion;
use super::read_replacement_relay;
use super::read_replacement_submission;
use super::reconcile_replacement_state;
use super::remove_file_if_exists;
use super::replace_bytes_atomically;
use super::replacement_authorization;
use super::sign_manifest;
use super::validate_identity;
use super::validate_manifest_identity;
use super::write_bytes_once_or_exact;
use super::FinalizedReplacementAuthorizationV1;
use super::FinalizedReplacementBindingV1;
use super::NodeHostIdentityV1;
use super::NodeHostPaths;
use super::NodeHostStateLock;
use super::ReplacementCandidateRecordV1;
use super::ReplacementCandidateRelayV1;
use super::ReplacementCandidateSubmissionV1;
use super::MAX_INITIALIZATION_MANIFEST_BYTES;

use crate::AuthorizedEnclaveClient;
use crate::GeneratedDcapQuoteV1;
use crate::NodeHostNoiseKey;
use crate::TransportError;
use alloy_primitives::keccak256;

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;
use outbe_primitives::tee_attestation_v1::AttestationOperationV1;
use outbe_primitives::tee_attestation_v1::DcapEvidenceV1;
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use std::fs;

use std::fs::File;

use std::path::Path;

/// Authenticated session to one staged replacement enclave. The normal startup
/// path continues to use the committed enclave until an I6 finalized-state
/// capability authorizes promotion.
pub struct ReplacementCandidateEnclaveV1 {
    client: AuthorizedEnclaveClient,
    manifest: EnclaveInitializationManifestV1,
}

impl ReplacementCandidateEnclaveV1 {
    #[must_use]
    pub const fn manifest(&self) -> &EnclaveInitializationManifestV1 {
        &self.manifest
    }

    pub fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1, TransportError> {
        let generated = self.client.generate_dcap_quote(intent)?;
        if intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement {
            let proof = generated
                .transition_key_ready_proof
                .as_ref()
                .ok_or_else(|| {
                    TransportError::Attestation(
                        "replacement candidate returned no transition key-ready proof".into(),
                    )
                })?;
            let expected_manifest_hash = self.manifest.authorization_hash().map_err(codec_error)?;
            if proof.candidate_manifest_hash != expected_manifest_hash {
                return Err(TransportError::Attestation(
                    "transition key-ready proof targets another candidate manifest".into(),
                ));
            }
        }
        Ok(generated)
    }

    pub fn request(
        &mut self,
        request: &crate::protocol::EnclaveRequest,
    ) -> Result<crate::protocol::EnclaveResponse, TransportError> {
        self.client.request(request)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ingest_finalized_admission_v1(
        &mut self,
        artifact: &[u8],
        anchor_outcome: &[u8],
        committee_transitions: &[Vec<u8>],
        finalized_admission_witness: &[u8],
        expected_intent_hash: B256,
        expected_tribute_offer_public: [u8; 32],
        expected_key_epoch: u64,
        expected_tribute_offer_epoch: u64,
    ) -> Result<[u8; 32], TransportError> {
        self.client.ingest_finalized_admission_v1(
            artifact,
            anchor_outcome,
            committee_transitions,
            finalized_admission_witness,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_finalized_admission_v1(
        &mut self,
        artifact: &[u8],
        anchor_outcome: &[u8],
        expected_intent_hash: B256,
        expected_tribute_offer_public: [u8; 32],
        expected_key_epoch: u64,
        expected_tribute_offer_epoch: u64,
    ) -> Result<B256, TransportError> {
        self.client.begin_finalized_admission_v1(
            artifact,
            anchor_outcome,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
        )
    }

    pub fn upload_finalized_admission_record_v1(
        &mut self,
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
        record: &[u8],
    ) -> Result<(), TransportError> {
        self.client
            .upload_finalized_admission_record_v1(request_hash, kind, record)
    }

    pub fn finish_finalized_admission_v1(
        &mut self,
        request_hash: B256,
        expected_tribute_offer_public: [u8; 32],
    ) -> Result<[u8; 32], TransportError> {
        self.client
            .finish_finalized_admission_v1(request_hash, expected_tribute_offer_public)
    }

    pub fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64], TransportError> {
        self.client.sign_registration_intent_dev_v1(intent)
    }
}

/// Stage one fresh enclave under the already committed NodeHost identity
/// and persistent NodeHost key. The committed enclave remains the normal
/// startup target.
pub fn prepare_node_host_enclave_replacement_candidate<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: NodeHostIdentityV1,
    sign_authorization: F,
) -> Result<ReplacementCandidateEnclaveV1, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    prepare_enclave_replacement_candidate(endpoint, node_data_dir, identity, sign_authorization)
}

/// Persist exact canonical replacement transaction material. An exact retry is
/// idempotent; any conflict is rejected so restart never silently changes the
/// quote, collateral or proof-of-possession signatures.
pub fn persist_replacement_candidate_submission(
    node_data_dir: &Path,
    evidence: &AttestationEvidenceV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> Result<ReplacementCandidateSubmissionV1, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "replacement submission requires committed and candidate NodeHost state".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    if !path_exists(&paths.replacement_candidate)? {
        return Err(TransportError::Codec(
            "replacement submission requires a durable candidate".into(),
        ));
    }
    let active = read_manifest(&paths.manifest)?;
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    validate_replacement_candidate_state(&candidate, &active, &node_host)?;

    let evidence_bytes = evidence.encode_canonical().map_err(codec_error)?;
    let intent = match evidence {
        AttestationEvidenceV1::Dcap(value)
            if is_candidate_promotion_operation(value.intent.operation) =>
        {
            validate_candidate_key_ready_proof(&candidate.manifest, value)?;
            &value.intent
        }
        AttestationEvidenceV1::GramineDirectDev(value)
            if value.intent.operation == AttestationOperationV1::RegisterEnclave
                && &value.dev_signature == enclave_signature =>
        {
            &value.intent
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::GramineDirectDev(_) => {
            return Err(TransportError::Codec(
                "candidate submission is not an allowed registration or successor operation".into(),
            ));
        }
    };
    candidate
        .manifest
        .validate_intent_binding(intent)
        .map_err(codec_error)?;
    if !intent.verify_node_signature(node_signature) {
        return Err(TransportError::Codec(
            "replacement submission node signature is invalid".into(),
        ));
    }
    if !intent.verify_enclave_signature(enclave_signature) {
        return Err(TransportError::Codec(
            "replacement submission enclave signature is invalid".into(),
        ));
    }
    let submission = ReplacementCandidateSubmissionV1 {
        evidence: evidence_bytes,
        node_signature: *node_signature,
        enclave_signature: *enclave_signature,
    };
    let bytes = submission.encode_canonical()?;
    if path_exists(&paths.replacement_submission)? {
        let durable = read_replacement_submission(&paths.replacement_submission)?;
        if durable == submission {
            return Ok(durable);
        }
        return Err(TransportError::Codec(
            "replacement material conflicts with the durable replacement submission".into(),
        ));
    }
    replace_bytes_atomically(
        &paths.replacement_submission,
        &paths.replacement_submission_next,
        &paths.replacement_write_scratch,
        &bytes,
        &paths.root,
    )?;
    Ok(submission)
}

/// Reload exact durable replacement transaction material after a relay or
/// NodeHost restart. Journal reconciliation completes only already-fsynced
/// candidate/submission writes and never promotes the active enclave.
pub fn load_replacement_candidate_submission(
    node_data_dir: &Path,
) -> Result<Option<ReplacementCandidateSubmissionV1>, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "replacement submission reload requires committed NodeHost state".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    if !path_exists(&paths.replacement_submission)? {
        return Ok(None);
    }
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    validate_durable_replacement_submission(&candidate.manifest, &submission)?;
    Ok(Some(submission))
}

/// Persist the exact signed transaction before the first relay attempt. The
/// transaction is inseparable from the already durable candidate submission.
pub fn persist_replacement_candidate_relay(
    node_data_dir: &Path,
    calldata_hash: B256,
    raw_transaction: &[u8],
) -> Result<ReplacementCandidateRelayV1, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    if !path_exists(&paths.replacement_candidate)? || !path_exists(&paths.replacement_submission)? {
        return Err(TransportError::Codec(
            "replacement relay requires durable candidate submission state".into(),
        ));
    }
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    validate_durable_replacement_submission(&candidate.manifest, &submission)?;
    if calldata_hash.is_zero() || raw_transaction.is_empty() {
        return Err(TransportError::Codec(
            "replacement relay transaction is incomplete".into(),
        ));
    }
    let relay = ReplacementCandidateRelayV1 {
        submission_hash: submission.submission_hash()?,
        calldata_hash,
        transaction_hash: keccak256(raw_transaction),
        raw_transaction: raw_transaction.to_vec(),
    };
    let bytes = relay.encode_canonical()?;
    if path_exists(&paths.replacement_relay)? {
        let durable = read_replacement_relay(&paths.replacement_relay)?;
        if durable == relay {
            return Ok(durable);
        }
        return Err(TransportError::Codec(
            "replacement transaction conflicts with the durable relay checkpoint".into(),
        ));
    }
    replace_bytes_atomically(
        &paths.replacement_relay,
        &paths.replacement_relay_next,
        &paths.replacement_write_scratch,
        &bytes,
        &paths.root,
    )?;
    Ok(relay)
}

/// Reload the byte-identical signed candidate transaction after restart.
pub fn load_replacement_candidate_relay(
    node_data_dir: &Path,
) -> Result<Option<ReplacementCandidateRelayV1>, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    if !path_exists(&paths.replacement_relay)? {
        return Ok(None);
    }
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    let relay = read_replacement_relay(&paths.replacement_relay)?;
    if relay.submission_hash != submission.submission_hash()? {
        return Err(TransportError::Codec(
            "replacement relay targets another durable submission".into(),
        ));
    }
    Ok(Some(relay))
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
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "replacement authorization requires committed NodeHost state".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    if !path_exists(&paths.replacement_candidate)? || !path_exists(&paths.replacement_submission)? {
        return Err(TransportError::Codec(
            "replacement authorization requires a complete durable candidate and submission".into(),
        ));
    }

    let active = read_manifest(&paths.manifest)?;
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    validate_replacement_candidate_state(&candidate, &active, &node_host)?;
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
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "replacement promotion requires committed NodeHost state".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let active = read_manifest(&paths.manifest)?;
    if active.node_host_noise_x25519 != node_host.public() {
        return Err(TransportError::Codec(
            "committed manifest does not match the persistent NodeHost key".into(),
        ));
    }
    let candidate_exists = path_exists(&paths.replacement_candidate)?;
    let submission_exists = path_exists(&paths.replacement_submission)?;
    if !candidate_exists && !submission_exists {
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
        return Err(TransportError::Codec(
            "no replacement candidate is staged for this finalized authorization".into(),
        ));
    }
    if candidate_exists != submission_exists {
        return Err(TransportError::Codec(
            "replacement candidate and submission durability state is incomplete".into(),
        ));
    }

    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    validate_replacement_candidate_state(&candidate, &active, &node_host)?;
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
        &manifest_bytes,
        MAX_INITIALIZATION_MANIFEST_BYTES,
        &paths.root,
        "next replacement manifest",
    )?;
    fs::rename(&paths.next_manifest, &paths.manifest)?;
    File::open(&paths.root)?.sync_all()?;
    remove_file_if_exists(&paths.replacement_relay)?;
    File::open(&paths.root)?.sync_all()?;
    remove_file_if_exists(&paths.replacement_submission)?;
    File::open(&paths.root)?.sync_all()?;
    remove_file_if_exists(&paths.replacement_candidate)?;
    File::open(&paths.root)?.sync_all()?;
    Ok(candidate.manifest)
}

fn prepare_enclave_replacement_candidate<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: NodeHostIdentityV1,
    sign_authorization: F,
) -> Result<ReplacementCandidateEnclaveV1, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    validate_identity(&identity)?;
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || path_exists(&paths.pending_manifest)? {
        return Err(TransportError::Codec(
            "replacement candidate requires one unambiguous committed NodeHost manifest".into(),
        ));
    }
    if !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "committed NodeHost manifest exists but its persistent Noise key is missing; refusing recovery"
                .into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let active = read_manifest(&paths.manifest)?;
    validate_manifest_identity(&active, &identity, &node_host)?;
    if path_exists(&paths.replacement_candidate)? {
        let record = read_replacement_candidate(&paths.replacement_candidate)?;
        validate_replacement_candidate(&record, &active, &identity, &node_host)?;
        if let Ok(client) =
            AuthorizedEnclaveClient::connect_endpoint(endpoint, &record.manifest, &node_host)
        {
            return Ok(ReplacementCandidateEnclaveV1 {
                client,
                manifest: record.manifest,
            });
        }
        if path_exists(&paths.replacement_submission)? {
            return Err(TransportError::Codec(
                "durable replacement submission exists but its candidate enclave cannot reconnect"
                    .into(),
            ));
        }
        let challenge = AuthorizedEnclaveClient::discover_endpoint(endpoint)?;
        let refreshed_manifest = EnclaveInitializationManifestV1 {
            chain_id: identity.network_binding.chain_id,
            genesis_hash: identity.network_binding.genesis_hash,
            attestation_mode: identity.network_binding.attestation_mode,
            node_id: identity.node_id(),
            initialization_challenge: challenge.challenge,
            node_host_noise_x25519: node_host.public(),
            recipient_x25519: challenge.recipient_x25519,
            attestation_ed25519: challenge.attestation_ed25519,
            noise_responder_x25519: challenge.noise_responder_x25519,
        };
        let refreshed_record = ReplacementCandidateRecordV1 {
            predecessor_manifest_hash: record.predecessor_manifest_hash,
            manifest: refreshed_manifest.clone(),
        };
        validate_replacement_candidate(&refreshed_record, &active, &identity, &node_host)?;
        if refreshed_manifest.recipient_x25519 != record.manifest.recipient_x25519
            || refreshed_manifest.attestation_ed25519 != record.manifest.attestation_ed25519
            || refreshed_manifest.noise_responder_x25519 != record.manifest.noise_responder_x25519
        {
            return Err(TransportError::Codec(
                "replacement endpoint changed candidate enclave identity during resume".into(),
            ));
        }
        replace_bytes_atomically(
            &paths.replacement_candidate,
            &paths.replacement_candidate_next,
            &paths.replacement_write_scratch,
            &refreshed_record.encode_canonical()?,
            &paths.root,
        )?;
        let signature = sign_manifest(&refreshed_manifest, &sign_authorization)?;
        let client = AuthorizedEnclaveClient::initialize_endpoint(
            endpoint,
            &refreshed_manifest,
            &signature,
            &node_host,
        )?;
        return Ok(ReplacementCandidateEnclaveV1 {
            client,
            manifest: refreshed_manifest,
        });
    }

    let challenge = AuthorizedEnclaveClient::discover_endpoint(endpoint)?;
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: identity.network_binding.chain_id,
        genesis_hash: identity.network_binding.genesis_hash,
        attestation_mode: identity.network_binding.attestation_mode,
        node_id: identity.node_id(),
        initialization_challenge: challenge.challenge,
        node_host_noise_x25519: node_host.public(),
        recipient_x25519: challenge.recipient_x25519,
        attestation_ed25519: challenge.attestation_ed25519,
        noise_responder_x25519: challenge.noise_responder_x25519,
    };
    validate_manifest_identity(&manifest, &identity, &node_host)?;
    let record = ReplacementCandidateRecordV1 {
        predecessor_manifest_hash: active.authorization_hash().map_err(codec_error)?,
        manifest: manifest.clone(),
    };
    validate_replacement_candidate(&record, &active, &identity, &node_host)?;
    replace_bytes_atomically(
        &paths.replacement_candidate,
        &paths.replacement_candidate_next,
        &paths.replacement_write_scratch,
        &record.encode_canonical()?,
        &paths.root,
    )?;
    let signature = sign_manifest(&manifest, &sign_authorization)?;
    let client =
        AuthorizedEnclaveClient::initialize_endpoint(endpoint, &manifest, &signature, &node_host)?;
    Ok(ReplacementCandidateEnclaveV1 { client, manifest })
}

fn validate_replacement_candidate(
    record: &ReplacementCandidateRecordV1,
    active: &EnclaveInitializationManifestV1,
    identity: &NodeHostIdentityV1,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    validate_manifest_identity(&record.manifest, identity, node_host)?;
    validate_replacement_candidate_state(record, active, node_host)
}

pub(super) fn validate_replacement_candidate_state(
    record: &ReplacementCandidateRecordV1,
    active: &EnclaveInitializationManifestV1,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    if active.node_host_noise_x25519 != node_host.public()
        || record.manifest.node_host_noise_x25519 != node_host.public()
        || record.manifest.chain_id != active.chain_id
        || record.manifest.genesis_hash != active.genesis_hash
        || record.manifest.node_id != active.node_id
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

pub(super) fn validate_durable_replacement_submission(
    manifest: &EnclaveInitializationManifestV1,
    submission: &ReplacementCandidateSubmissionV1,
) -> Result<RegistrationIntentV1, TransportError> {
    let evidence =
        AttestationEvidenceV1::decode_canonical(&submission.evidence).map_err(codec_error)?;
    let intent = match evidence {
        AttestationEvidenceV1::Dcap(value)
            if is_candidate_promotion_operation(value.intent.operation) =>
        {
            validate_candidate_key_ready_proof(manifest, &value)?;
            value.intent
        }
        AttestationEvidenceV1::GramineDirectDev(value)
            if value.intent.operation == AttestationOperationV1::RegisterEnclave
                && value.dev_signature == submission.enclave_signature =>
        {
            value.intent
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::GramineDirectDev(_) => {
            return Err(TransportError::Codec(
                "durable submission is not an allowed registration or successor operation".into(),
            ));
        }
    };
    manifest
        .validate_intent_binding(&intent)
        .map_err(codec_error)?;
    if !intent.verify_node_signature(&submission.node_signature)
        || !intent.verify_enclave_signature(&submission.enclave_signature)
    {
        return Err(TransportError::Codec(
            "durable replacement submission proof of possession is invalid".into(),
        ));
    }
    Ok(intent)
}

fn validate_candidate_key_ready_proof(
    manifest: &EnclaveInitializationManifestV1,
    evidence: &DcapEvidenceV1,
) -> Result<(), TransportError> {
    if evidence.intent.operation != AttestationOperationV1::TransitionEnclaveMeasurement {
        return Ok(());
    }
    let proof = evidence
        .transition_key_ready_proof
        .as_ref()
        .ok_or_else(|| {
            TransportError::Codec("transition evidence is missing its key-ready proof".into())
        })?;
    let expected_manifest_hash = manifest.authorization_hash().map_err(codec_error)?;
    if proof.candidate_manifest_hash != expected_manifest_hash {
        return Err(TransportError::Codec(
            "transition key-ready proof targets another durable candidate manifest".into(),
        ));
    }
    Ok(())
}

fn is_candidate_promotion_operation(operation: AttestationOperationV1) -> bool {
    matches!(
        operation,
        AttestationOperationV1::RegisterEnclave
            | AttestationOperationV1::ReplaceEnclaveBinding
            | AttestationOperationV1::TransitionEnclaveMeasurement
    )
}

fn validate_finalized_replacement_binding(
    intent: &RegistrationIntentV1,
    finalized: &FinalizedReplacementBindingV1,
) -> Result<(), TransportError> {
    let node_id_hash = intent.node_id.node_id_hash().map_err(codec_error)?;
    let intent_hash = intent.intent_hash().map_err(codec_error)?;
    let expected_chain_id = intent.chain_id;
    let view_is_well_formed = finalized.view.chain_id != [0; 32]
        && !finalized.view.genesis_hash.is_zero()
        && finalized.view.block_number != 0
        && !finalized.view.block_hash.is_zero()
        && !finalized.view.state_root.is_zero()
        && finalized.view.consensus_timestamp != 0;
    let binding_is_well_formed = !finalized.node_id_hash.is_zero()
        && !finalized.enclave_id.is_zero()
        && !finalized.binding_id.is_zero()
        && !finalized.intent_hash.is_zero()
        && finalized.binding_version != 0
        && finalized.registration_version != 0
        && finalized.valid_until > finalized.view.consensus_timestamp
        && finalized.recipient_x25519 != [0; 32]
        && finalized.attestation_ed25519 != [0; 32]
        && finalized.noise_responder_x25519 != [0; 32]
        && !finalized.node_host_authorization_hash.is_zero();
    let exact_match = finalized.view.chain_id == expected_chain_id
        && finalized.view.genesis_hash == intent.genesis_hash
        && finalized.node_id_hash == node_id_hash
        && finalized.enclave_id == intent.enclave_id
        && finalized.binding_id == intent.binding_id
        && finalized.intent_hash == intent_hash
        && finalized.binding_version == intent.binding_version
        && finalized.registration_version == intent.registration_version
        && finalized.valid_until == intent.requested_valid_until
        && finalized.recipient_x25519 == intent.recipient_x25519
        && finalized.attestation_ed25519 == intent.attestation_ed25519
        && finalized.noise_responder_x25519 == intent.noise_responder_x25519
        && finalized.node_host_authorization_hash == intent.node_host_authorization_hash;
    if !view_is_well_formed || !binding_is_well_formed || !exact_match {
        return Err(TransportError::Codec(
            "finalized Registry binding does not match the durable replacement intent".into(),
        ));
    }
    Ok(())
}
