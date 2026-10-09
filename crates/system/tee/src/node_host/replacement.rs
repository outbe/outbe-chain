use super::codec_error;
use super::durable_submission::{
    candidate_promotion_intent, verify_bound_possession, verify_durable_submission,
    DurableSubmission, DurableSubmissionKind, PossessionErrors,
};
use super::filesystem::remove_file_and_sync_directory;
use super::identity::{
    discover_initialization_manifest, initialize_authorized_endpoint, manifest_for_challenge,
    read_committed_identity,
};
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
use super::validate_identity;
use super::validate_manifest_identity;
use super::write_bytes_once_or_exact;
use super::BoundedRecordBytes;
use super::FinalizedReplacementAuthorizationV1;
use super::FinalizedReplacementBindingV1;
use super::NodeHostIdentityV1;
use super::NodeHostPaths;
use super::NodeHostStateLock;
use super::ReplacementCandidateRecordV1;
use super::ReplacementCandidateRelayV1;
use super::ReplacementCandidateSubmissionV1;
use super::MAX_INITIALIZATION_MANIFEST_BYTES;

use crate::finalized_admission::{FinalizedAdmissionBeginInputV1, FinalizedAdmissionIngestInputV1};
use crate::AuthorizedEnclaveClient;
use crate::GeneratedDcapQuoteV1;
use crate::NodeHostNoiseKey;
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

    pub fn ingest_upgrade_key_v1(
        &mut self,
        proof: &crate::upgrade_transfer::UpgradeKeyProofV1,
        artifact: &[u8],
    ) -> Result<crate::protocol::EnclaveResponse, TransportError> {
        self.client.transfer_upgrade_key_v1(proof, artifact, false)
    }

    pub fn ingest_finalized_admission_v1(
        &mut self,
        input: FinalizedAdmissionIngestInputV1<'_>,
    ) -> Result<[u8; 32], TransportError> {
        self.client.ingest_finalized_admission_v1(input)
    }

    pub fn begin_finalized_admission_v1(
        &mut self,
        input: FinalizedAdmissionBeginInputV1<'_>,
    ) -> Result<B256, TransportError> {
        self.client.begin_finalized_admission_v1(input)
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

    pub fn generate_transition_evidence_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<AttestationEvidenceV1, TransportError> {
        let bytes = intent.encode_canonical().map_err(codec_error)?;
        let response = self.client.request(
            &crate::protocol::EnclaveRequest::GenerateTransitionEvidenceDevV1 { intent: bytes },
        )?;
        let crate::protocol::EnclaveResponse::TransitionEvidenceDevV1 { evidence } = response
        else {
            return Err(TransportError::UnexpectedResponse);
        };
        let evidence = AttestationEvidenceV1::decode_canonical(&evidence).map_err(codec_error)?;
        let AttestationEvidenceV1::GramineDirectDev(dev) = &evidence else {
            return Err(TransportError::UnexpectedResponse);
        };
        if &dev.intent != intent || !intent.verify_enclave_signature(&dev.dev_signature) {
            return Err(TransportError::Attestation(
                "DirectDev transition response differs from request".into(),
            ));
        }
        let proof = evidence
            .transition_key_ready_proof()
            .ok_or_else(|| TransportError::Attestation("missing key-ready proof".into()))?;
        proof
            .verify_for_transition(intent, proof.resident_offer_public)
            .map_err(codec_error)?;
        if proof.candidate_manifest_hash
            != self.manifest().authorization_hash().map_err(codec_error)?
        {
            return Err(TransportError::Attestation(
                "transition proof targets another manifest".into(),
            ));
        }
        Ok(evidence)
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

// A replacement operation owns this lock for its entire state transition.
struct LockedReplacementState {
    paths: NodeHostPaths,
    node_host: NodeHostNoiseKey,
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
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
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
    node_host: &NodeHostNoiseKey,
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
    let (paths, _state_lock) = lock_node_host_state(node_data_dir)?;
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
    let preparation = CandidatePreparation {
        endpoint,
        paths: &paths,
        identity: &identity,
        node_host: &node_host,
        active: &active,
        sign_authorization: &sign_authorization,
    };
    if path_exists(&paths.replacement_candidate)? {
        let record = read_replacement_candidate(&paths.replacement_candidate)?;
        validate_replacement_candidate(&record, &active, &identity, &node_host)?;
        return preparation.resume(record);
    }
    preparation.create()
}

struct CandidatePreparation<'a, F> {
    endpoint: &'a str,
    paths: &'a NodeHostPaths,
    identity: &'a NodeHostIdentityV1,
    node_host: &'a NodeHostNoiseKey,
    active: &'a EnclaveInitializationManifestV1,
    sign_authorization: &'a F,
}

impl<F> CandidatePreparation<'_, F>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    fn resume(
        &self,
        record: ReplacementCandidateRecordV1,
    ) -> Result<ReplacementCandidateEnclaveV1, TransportError> {
        if let Ok(client) = AuthorizedEnclaveClient::connect_endpoint(
            self.endpoint,
            &record.manifest,
            self.node_host,
        ) {
            return Ok(ReplacementCandidateEnclaveV1 {
                client,
                manifest: record.manifest,
            });
        }
        if path_exists(&self.paths.replacement_submission)? {
            return Err(TransportError::Codec(
                "durable replacement submission exists but its candidate enclave cannot reconnect"
                    .into(),
            ));
        }
        let refreshed_manifest = self.refresh_candidate(&record)?;
        let client = initialize_authorized_endpoint(
            self.endpoint,
            &refreshed_manifest,
            self.node_host,
            self.sign_authorization,
        )?;
        Ok(ReplacementCandidateEnclaveV1 {
            client,
            manifest: refreshed_manifest,
        })
    }

    fn refresh_candidate(
        &self,
        record: &ReplacementCandidateRecordV1,
    ) -> Result<EnclaveInitializationManifestV1, TransportError> {
        let challenge = AuthorizedEnclaveClient::discover_endpoint(self.endpoint)?;
        let refreshed_manifest = manifest_for_challenge(self.identity, self.node_host, &challenge);
        let refreshed_record = ReplacementCandidateRecordV1 {
            predecessor_manifest_hash: record.predecessor_manifest_hash,
            manifest: refreshed_manifest.clone(),
        };
        validate_replacement_candidate(
            &refreshed_record,
            self.active,
            self.identity,
            self.node_host,
        )?;
        if refreshed_manifest.recipient_x25519 != record.manifest.recipient_x25519
            || refreshed_manifest.attestation_ed25519 != record.manifest.attestation_ed25519
            || refreshed_manifest.noise_responder_x25519 != record.manifest.noise_responder_x25519
        {
            return Err(TransportError::Codec(
                "replacement endpoint changed candidate enclave identity during resume".into(),
            ));
        }
        replace_bytes_atomically(
            &self.paths.replacement_candidate,
            &self.paths.replacement_candidate_next,
            &self.paths.replacement_write_scratch,
            &refreshed_record.encode_canonical()?,
            &self.paths.root,
        )?;
        Ok(refreshed_manifest)
    }

    fn create(&self) -> Result<ReplacementCandidateEnclaveV1, TransportError> {
        let manifest =
            discover_initialization_manifest(self.endpoint, self.identity, self.node_host)?;
        let record = ReplacementCandidateRecordV1 {
            predecessor_manifest_hash: self.active.authorization_hash().map_err(codec_error)?,
            manifest: manifest.clone(),
        };
        validate_replacement_candidate(&record, self.active, self.identity, self.node_host)?;
        replace_bytes_atomically(
            &self.paths.replacement_candidate,
            &self.paths.replacement_candidate_next,
            &self.paths.replacement_write_scratch,
            &record.encode_canonical()?,
            &self.paths.root,
        )?;
        let client = initialize_authorized_endpoint(
            self.endpoint,
            &manifest,
            self.node_host,
            self.sign_authorization,
        )?;
        Ok(ReplacementCandidateEnclaveV1 { client, manifest })
    }
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
    node_host: &NodeHostNoiseKey,
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

pub(super) fn validate_direct_dev_transition_proof(
    manifest: &EnclaveInitializationManifestV1,
    evidence: &outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1,
) -> Result<(), TransportError> {
    let proof = evidence
        .transition_key_ready_proof
        .as_ref()
        .ok_or_else(|| TransportError::Codec("transition proof missing".into()))?;
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
