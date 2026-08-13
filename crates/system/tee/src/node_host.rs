//! Persistent host-side authorization for one production node enclave.
//!
//! The private NodeHost Noise key is write-once. A canonical public manifest is
//! first written as `pending`, committed by the enclave, then promoted to the
//! restart record. A crash after enclave commit but before promotion is closed
//! by reconnecting with the pending record and promoting it only on success.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256, U256};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationOperationV1, DcapEvidenceV1, EnclaveInitializationManifestV1,
    NodeIdV1, RegistrationIntentV1, MAX_ATTESTATION_EVIDENCE_BYTES,
};

use crate::{
    remote_session::FinalizedRegistryViewV1, AuthorizedEnclaveClient, GeneratedDcapQuoteV1,
    NodeHostNoiseKey, TransportError,
};

pub const NODE_HOST_DIRECTORY_V1: &str = "tee-node-host-v1";
pub const NODE_HOST_NOISE_KEY_V1: &str = "noise-initiator.key";
pub const NODE_HOST_MANIFEST_V1: &str = "initialization-manifest.bin";
const NODE_HOST_PENDING_MANIFEST_V1: &str = "initialization-manifest.pending";
pub const NODE_HOST_REPLACEMENT_CANDIDATE_V1: &str = "replacement-candidate.v1";
pub const NODE_HOST_REPLACEMENT_SUBMISSION_V1: &str = "replacement-submission.v1";
pub const NODE_HOST_REPLACEMENT_PROMOTION_V1: &str = "replacement-promotion.v1";
const NODE_HOST_NEXT_MANIFEST_V1: &str = "initialization-manifest.next";
const NODE_HOST_REPLACEMENT_CANDIDATE_NEXT_V1: &str = "replacement-candidate.next";
const NODE_HOST_REPLACEMENT_SUBMISSION_NEXT_V1: &str = "replacement-submission.next";
const NODE_HOST_REPLACEMENT_PROMOTION_NEXT_V1: &str = "replacement-promotion.next";
const NODE_HOST_STATE_LOCK_V1: &str = "state.lock";
const NODE_HOST_REPLACEMENT_WRITE_SCRATCH_V1: &str = "replacement-write.tmp";
const MAX_INITIALIZATION_MANIFEST_BYTES: u64 = 512;
const REPLACEMENT_CANDIDATE_VERSION_V1: u8 = 1;
const MAX_REPLACEMENT_CANDIDATE_BYTES: u64 = 1 + 32 + 2 + MAX_INITIALIZATION_MANIFEST_BYTES;
const REPLACEMENT_SUBMISSION_VERSION_V1: u8 = 1;
const MAX_REPLACEMENT_SUBMISSION_BYTES: u64 =
    1 + 4 + MAX_ATTESTATION_EVIDENCE_BYTES as u64 + 65 + 64;
const REPLACEMENT_PROMOTION_VERSION_V1: u8 = 1;
const REPLACEMENT_PROMOTION_BYTES: u64 = 1 + 32 + 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeHostIdentityV1 {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub reth_p2p_public: [u8; 33],
}

impl NodeHostIdentityV1 {
    fn chain_id_word(&self) -> [u8; 32] {
        U256::from(self.chain_id).to_be_bytes()
    }

    fn node_id(&self) -> NodeIdV1 {
        NodeIdV1 {
            reth_p2p_public: self.reth_p2p_public,
        }
    }
}

/// Authenticated session to one staged replacement enclave. The normal startup
/// path continues to use the committed enclave until an I6 finalized-state
/// capability authorizes promotion.
pub struct ReplacementCandidateEnclaveV1 {
    client: AuthorizedEnclaveClient,
    manifest: EnclaveInitializationManifestV1,
}

/// Exact durable transaction material returned for relay retries. The evidence
/// already contains the canonical replacement intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementCandidateSubmissionV1 {
    evidence: Vec<u8>,
    node_signature: [u8; 65],
    enclave_signature: [u8; 64],
}

/// Opaque authority issued only after I6 verifies an exact finalized registry
/// binding. I5 defines and consumes the capability but has no production
/// constructor for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedReplacementAuthorizationV1 {
    intent_hash: B256,
    candidate_manifest_hash: B256,
}

/// Exact replacement binding authenticated at one consensus-finalized Registry
/// state. Constructing the opaque promotion capability additionally proves
/// that these fields match the durable candidate and evidence byte-for-byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedReplacementBindingV1 {
    pub view: FinalizedRegistryViewV1,
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub binding_id: B256,
    pub intent_hash: B256,
    pub binding_version: u64,
    pub registration_version: u64,
    pub valid_until: u64,
    pub recipient_x25519: [u8; 32],
    pub attestation_ed25519: [u8; 32],
    pub noise_responder_x25519: [u8; 32],
    pub node_host_authorization_hash: B256,
}

impl FinalizedReplacementAuthorizationV1 {
    fn encode_canonical(self) -> [u8; REPLACEMENT_PROMOTION_BYTES as usize] {
        let mut out = [0_u8; REPLACEMENT_PROMOTION_BYTES as usize];
        out[0] = REPLACEMENT_PROMOTION_VERSION_V1;
        out[1..33].copy_from_slice(self.intent_hash.as_slice());
        out[33..].copy_from_slice(self.candidate_manifest_hash.as_slice());
        out
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() != REPLACEMENT_PROMOTION_BYTES as usize
            || input[0] != REPLACEMENT_PROMOTION_VERSION_V1
        {
            return Err(TransportError::Codec(
                "replacement promotion receipt framing is invalid".into(),
            ));
        }
        Ok(Self {
            intent_hash: B256::from_slice(&input[1..33]),
            candidate_manifest_hash: B256::from_slice(&input[33..]),
        })
    }

    #[cfg(test)]
    fn for_test(intent_hash: B256, candidate_manifest_hash: B256) -> Self {
        Self {
            intent_hash,
            candidate_manifest_hash,
        }
    }
}

impl ReplacementCandidateSubmissionV1 {
    #[must_use]
    pub fn evidence(&self) -> &[u8] {
        &self.evidence
    }

    #[must_use]
    pub const fn node_signature(&self) -> &[u8; 65] {
        &self.node_signature
    }

    #[must_use]
    pub const fn enclave_signature(&self) -> &[u8; 64] {
        &self.enclave_signature
    }

    pub fn submission_hash(&self) -> Result<B256, TransportError> {
        Ok(keccak256(self.encode_canonical()?))
    }

    fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        let evidence_len = u32::try_from(self.evidence.len()).map_err(|_| {
            TransportError::Codec("replacement submission evidence length overflow".into())
        })?;
        let capacity = 134_usize.checked_add(self.evidence.len()).ok_or_else(|| {
            TransportError::Codec("replacement submission allocation length overflow".into())
        })?;
        let mut out = Vec::with_capacity(capacity);
        out.push(REPLACEMENT_SUBMISSION_VERSION_V1);
        out.extend_from_slice(&evidence_len.to_be_bytes());
        out.extend_from_slice(&self.evidence);
        out.extend_from_slice(&self.node_signature);
        out.extend_from_slice(&self.enclave_signature);
        Ok(out)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() < 134
            || u64::try_from(input.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_SUBMISSION_BYTES
            || input[0] != REPLACEMENT_SUBMISSION_VERSION_V1
        {
            return Err(TransportError::Codec(
                "replacement submission framing is invalid".into(),
            ));
        }
        let evidence_len =
            usize::try_from(u32::from_be_bytes([input[1], input[2], input[3], input[4]])).map_err(
                |_| TransportError::Codec("replacement evidence length overflow".into()),
            )?;
        let expected_len = 134_usize.checked_add(evidence_len).ok_or_else(|| {
            TransportError::Codec("replacement submission length overflow".into())
        })?;
        if evidence_len > MAX_ATTESTATION_EVIDENCE_BYTES || input.len() != expected_len {
            return Err(TransportError::Codec(
                "replacement submission evidence length is non-canonical".into(),
            ));
        }
        let evidence_end = 5 + evidence_len;
        let evidence = input[5..evidence_end].to_vec();
        AttestationEvidenceV1::decode_canonical(&evidence).map_err(codec_error)?;
        let node_signature = input[evidence_end..evidence_end + 65]
            .try_into()
            .map_err(|_| TransportError::Codec("replacement node signature length".into()))?;
        let enclave_signature = input[evidence_end + 65..]
            .try_into()
            .map_err(|_| TransportError::Codec("replacement enclave signature length".into()))?;
        Ok(Self {
            evidence,
            node_signature,
            enclave_signature,
        })
    }
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
}

struct ReplacementCandidateRecordV1 {
    predecessor_manifest_hash: B256,
    manifest: EnclaveInitializationManifestV1,
}

impl ReplacementCandidateRecordV1 {
    fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        let manifest = self
            .manifest
            .encode_canonical()
            .map_err(|error| TransportError::Codec(error.to_string()))?;
        let manifest_len = u16::try_from(manifest.len()).map_err(|_| {
            TransportError::Codec("replacement candidate manifest exceeds its wire field".into())
        })?;
        let mut out = Vec::with_capacity(35 + manifest.len());
        out.push(REPLACEMENT_CANDIDATE_VERSION_V1);
        out.extend_from_slice(self.predecessor_manifest_hash.as_slice());
        out.extend_from_slice(&manifest_len.to_be_bytes());
        out.extend_from_slice(&manifest);
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_CANDIDATE_BYTES {
            return Err(TransportError::Codec(
                "replacement candidate record exceeds its fixed cap".into(),
            ));
        }
        Ok(out)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() < 35
            || u64::try_from(input.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_CANDIDATE_BYTES
        {
            return Err(TransportError::Codec(
                "replacement candidate record length is invalid".into(),
            ));
        }
        if input[0] != REPLACEMENT_CANDIDATE_VERSION_V1 {
            return Err(TransportError::Codec(
                "replacement candidate record version is unsupported".into(),
            ));
        }
        let predecessor_manifest_hash = B256::from_slice(&input[1..33]);
        let manifest_len = usize::from(u16::from_be_bytes([input[33], input[34]]));
        if manifest_len > usize::try_from(MAX_INITIALIZATION_MANIFEST_BYTES).unwrap_or(usize::MAX)
            || input.len() != 35 + manifest_len
        {
            return Err(TransportError::Codec(
                "replacement candidate manifest length is non-canonical".into(),
            ));
        }
        let manifest =
            EnclaveInitializationManifestV1::decode_canonical(&input[35..]).map_err(codec_error)?;
        Ok(Self {
            predecessor_manifest_hash,
            manifest,
        })
    }
}

/// Connect to the one initialized NodeHost enclave, or perform its one-time
/// initialization when no committed host manifest exists.
///
/// `node_data_dir` is the resolved chain-specific reth data directory. The
/// function owns only its fixed `tee-node-host-v1` child. A committed manifest
/// is never replaced: losing or replacing the enclave identity is an explicit
/// operator decision, not an implicit startup recovery path.
pub fn connect_or_initialize_node_host_enclave<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: NodeHostIdentityV1,
    sign_authorization: F,
) -> Result<AuthorizedEnclaveClient, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    connect_or_initialize_enclave(endpoint, node_data_dir, identity, sign_authorization)
}

/// Reconnect to the one already committed NodeHost identity. This path never
/// creates state and is used by later startup stages after the node entrypoint
/// has resolved and committed the persistent Reth P2P identity.
pub fn connect_committed_node_host_enclave(
    endpoint: &str,
    node_data_dir: &Path,
) -> Result<AuthorizedEnclaveClient, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "one committed production NodeHost manifest is required".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    AuthorizedEnclaveClient::connect_endpoint(endpoint, &manifest, &node_host)
}

/// Load the one committed production manifest after applying the same bounded,
/// owner-only NodeHost state checks used by startup. Missing, pending or
/// inconsistent state is an error; this function never creates or recovers an
/// enclave identity.
pub fn load_committed_enclave_manifest_v1(
    node_data_dir: &Path,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)?
        || !path_exists(&paths.noise_key)?
        || path_exists(&paths.pending_manifest)?
    {
        return Err(TransportError::Codec(
            "one committed production NodeHost manifest is required".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    if manifest.node_host_noise_x25519 != node_host.public() {
        return Err(TransportError::Codec(
            "committed manifest does not match the persistent NodeHost key".into(),
        ));
    }
    Ok(manifest)
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
    let dcap = match evidence {
        AttestationEvidenceV1::Dcap(value)
            if is_candidate_successor_operation(value.intent.operation) =>
        {
            value
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::GramineDirectDev(_) => {
            return Err(TransportError::Codec(
                "candidate submission must contain DCAP replacement or measurement-transition evidence"
                    .into(),
            ))
        }
    };
    validate_candidate_key_ready_proof(&candidate.manifest, dcap)?;
    let intent = &dcap.intent;
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
            chain_id: identity.chain_id_word(),
            genesis_hash: identity.genesis_hash,
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
        chain_id: identity.chain_id_word(),
        genesis_hash: identity.genesis_hash,
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

fn codec_error(error: outbe_primitives::tee_attestation_v1::CodecError) -> TransportError {
    TransportError::Codec(error.to_string())
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

fn validate_replacement_candidate_state(
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

fn connect_or_initialize_enclave<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: NodeHostIdentityV1,
    sign_authorization: F,
) -> Result<AuthorizedEnclaveClient, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    validate_identity(&identity)?;
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;

    let committed_exists = path_exists(&paths.manifest)?;
    let pending_exists = path_exists(&paths.pending_manifest)?;
    let key_exists = path_exists(&paths.noise_key)?;
    if (committed_exists || pending_exists) && !key_exists {
        return Err(TransportError::Codec(
            "NodeHost manifest exists but its persistent Noise key is missing; refusing implicit recovery"
                .into(),
        ));
    }
    if committed_exists && pending_exists {
        return Err(TransportError::Codec(
            "both committed and pending NodeHost manifests exist; startup state is ambiguous"
                .into(),
        ));
    }

    let node_host = if key_exists {
        NodeHostNoiseKey::load(&paths.noise_key)?
    } else {
        NodeHostNoiseKey::create_new(&paths.noise_key)?
    };
    if committed_exists {
        reconcile_replacement_state(&paths, &node_host)?;
    }

    if committed_exists {
        let manifest = read_manifest(&paths.manifest)?;
        validate_manifest_identity(&manifest, &identity, &node_host)?;
        return AuthorizedEnclaveClient::connect_endpoint(endpoint, &manifest, &node_host);
    }

    if pending_exists {
        let manifest = read_manifest(&paths.pending_manifest)?;
        validate_manifest_identity(&manifest, &identity, &node_host)?;
        if let Ok(client) =
            AuthorizedEnclaveClient::connect_endpoint(endpoint, &manifest, &node_host)
        {
            promote_pending_manifest(&paths)?;
            return Ok(client);
        }
        let signature = sign_manifest(&manifest, &sign_authorization)?;
        let client = AuthorizedEnclaveClient::initialize_endpoint(
            endpoint, &manifest, &signature, &node_host,
        )?;
        promote_pending_manifest(&paths)?;
        return Ok(client);
    }

    let challenge = AuthorizedEnclaveClient::discover_endpoint(endpoint)?;
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: identity.chain_id_word(),
        genesis_hash: identity.genesis_hash,
        node_id: identity.node_id(),
        initialization_challenge: challenge.challenge,
        node_host_noise_x25519: node_host.public(),
        recipient_x25519: challenge.recipient_x25519,
        attestation_ed25519: challenge.attestation_ed25519,
        noise_responder_x25519: challenge.noise_responder_x25519,
    };
    validate_manifest_identity(&manifest, &identity, &node_host)?;
    write_manifest_once(&paths.pending_manifest, &manifest, &paths.root)?;
    let signature = sign_manifest(&manifest, &sign_authorization)?;
    let client =
        AuthorizedEnclaveClient::initialize_endpoint(endpoint, &manifest, &signature, &node_host)?;
    promote_pending_manifest(&paths)?;
    Ok(client)
}

fn validate_identity(identity: &NodeHostIdentityV1) -> Result<(), TransportError> {
    if identity.chain_id == 0 || identity.genesis_hash.is_zero() {
        return Err(TransportError::Codec(
            "NodeHost identity contains a zero chain identity".into(),
        ));
    }
    identity
        .node_id()
        .node_id_hash()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    Ok(())
}

fn validate_manifest_identity(
    manifest: &EnclaveInitializationManifestV1,
    identity: &NodeHostIdentityV1,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    manifest
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    if manifest.chain_id != identity.chain_id_word()
        || manifest.genesis_hash != identity.genesis_hash
        || manifest.node_id != identity.node_id()
        || manifest.node_host_noise_x25519 != node_host.public()
    {
        return Err(TransportError::Codec(
            "persisted NodeHost manifest does not match this node startup identity".into(),
        ));
    }
    Ok(())
}

fn sign_manifest<F>(
    manifest: &EnclaveInitializationManifestV1,
    sign_authorization: &F,
) -> Result<[u8; 65], TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    let hash = manifest
        .authorization_hash()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    let signature = sign_authorization(hash).map_err(TransportError::Codec)?;
    if !manifest.verify_node_signature(&signature) {
        return Err(TransportError::Codec(
            "node signer produced an invalid NodeHost manifest signature".into(),
        ));
    }
    Ok(signature)
}

fn read_manifest(path: &Path) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let bytes = read_owned_bounded_file(path, MAX_INITIALIZATION_MANIFEST_BYTES, "manifest")?;
    EnclaveInitializationManifestV1::decode_canonical(&bytes)
        .map_err(|error| TransportError::Codec(error.to_string()))
}

fn read_replacement_candidate(path: &Path) -> Result<ReplacementCandidateRecordV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        MAX_REPLACEMENT_CANDIDATE_BYTES,
        "replacement candidate",
    )?;
    ReplacementCandidateRecordV1::decode_canonical(&bytes)
}

fn read_replacement_submission(
    path: &Path,
) -> Result<ReplacementCandidateSubmissionV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        MAX_REPLACEMENT_SUBMISSION_BYTES,
        "replacement submission",
    )?;
    ReplacementCandidateSubmissionV1::decode_canonical(&bytes)
}

fn read_replacement_promotion(
    path: &Path,
) -> Result<FinalizedReplacementAuthorizationV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        REPLACEMENT_PROMOTION_BYTES,
        "replacement promotion receipt",
    )?;
    FinalizedReplacementAuthorizationV1::decode_canonical(&bytes)
}

fn validate_durable_replacement_submission(
    manifest: &EnclaveInitializationManifestV1,
    submission: &ReplacementCandidateSubmissionV1,
) -> Result<RegistrationIntentV1, TransportError> {
    let evidence =
        AttestationEvidenceV1::decode_canonical(&submission.evidence).map_err(codec_error)?;
    let dcap = match evidence {
        AttestationEvidenceV1::Dcap(value)
            if is_candidate_successor_operation(value.intent.operation) =>
        {
            value
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::GramineDirectDev(_) => {
            return Err(TransportError::Codec(
                "durable submission is not DCAP replacement or measurement-transition evidence"
                    .into(),
            ))
        }
    };
    validate_candidate_key_ready_proof(manifest, &dcap)?;
    let intent = dcap.intent;
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

fn is_candidate_successor_operation(operation: AttestationOperationV1) -> bool {
    matches!(
        operation,
        AttestationOperationV1::ReplaceEnclaveBinding
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

fn read_owned_bounded_file(
    path: &Path,
    maximum_len: u64,
    label: &'static str,
) -> Result<Vec<u8>, TransportError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.len() > maximum_len
    {
        return Err(TransportError::Codec(format!(
            "NodeHost {label} must be an owner-only bounded regular file"
        )));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| TransportError::Codec(format!("NodeHost {label} length overflow")))?;
    let read_limit = maximum_len
        .checked_add(1)
        .ok_or_else(|| TransportError::Codec(format!("NodeHost {label} read limit overflow")))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(read_limit).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_len {
        return Err(TransportError::Codec(format!(
            "NodeHost {label} grew beyond its byte bound while being read"
        )));
    }
    Ok(bytes)
}

struct NodeHostStateLock {
    _file: File,
}

impl NodeHostStateLock {
    fn acquire(path: &Path) -> Result<Self, TransportError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(TransportError::Codec(
                "NodeHost state lock must be an owner-only regular file".into(),
            ));
        }
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(std::io::Error::other)?;
        Ok(Self { _file: file })
    }
}

fn write_manifest_once(
    path: &Path,
    manifest: &EnclaveInitializationManifestV1,
    directory: &Path,
) -> Result<(), TransportError> {
    let bytes = manifest
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn write_bytes_once(path: &Path, bytes: &[u8], directory: &Path) -> Result<(), TransportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn write_bytes_once_or_exact(
    path: &Path,
    scratch_path: &Path,
    bytes: &[u8],
    maximum_len: u64,
    directory: &Path,
    label: &'static str,
) -> Result<(), TransportError> {
    if path_exists(path)? {
        if read_owned_bounded_file(path, maximum_len, label)? == bytes {
            return Ok(());
        }
        return Err(TransportError::Codec(format!(
            "durable NodeHost {label} conflicts with the requested value"
        )));
    }
    stage_complete_bytes(path, scratch_path, bytes, directory)
}

fn replace_bytes_atomically(
    path: &Path,
    next_path: &Path,
    scratch_path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), TransportError> {
    remove_file_if_exists(next_path)?;
    stage_complete_bytes(next_path, scratch_path, bytes, directory)?;
    fs::rename(next_path, path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn stage_complete_bytes(
    path: &Path,
    scratch_path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), TransportError> {
    remove_file_if_exists(scratch_path)?;
    write_bytes_once(scratch_path, bytes, directory)?;
    fs::rename(scratch_path, path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<(), TransportError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn replacement_authorization(
    candidate: &ReplacementCandidateRecordV1,
    submission: &ReplacementCandidateSubmissionV1,
) -> Result<FinalizedReplacementAuthorizationV1, TransportError> {
    let intent = validate_durable_replacement_submission(&candidate.manifest, submission)?;
    Ok(FinalizedReplacementAuthorizationV1 {
        intent_hash: intent.intent_hash().map_err(codec_error)?,
        candidate_manifest_hash: candidate
            .manifest
            .authorization_hash()
            .map_err(codec_error)?,
    })
}

fn validate_candidate_refresh_pair(
    candidate: &ReplacementCandidateRecordV1,
    candidate_next: &ReplacementCandidateRecordV1,
) -> Result<(), TransportError> {
    if candidate_next.predecessor_manifest_hash != candidate.predecessor_manifest_hash
        || candidate_next
            .manifest
            .node_host_authorization_hash()
            .map_err(codec_error)?
            != candidate
                .manifest
                .node_host_authorization_hash()
                .map_err(codec_error)?
        || candidate_next.manifest.enclave_id().map_err(codec_error)?
            != candidate.manifest.enclave_id().map_err(codec_error)?
    {
        return Err(TransportError::Codec(
            "candidate refresh journal changes replacement identity".into(),
        ));
    }
    Ok(())
}

fn reconcile_replacement_state(
    paths: &NodeHostPaths,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    if path_exists(&paths.replacement_write_scratch)? {
        remove_file_if_exists(&paths.replacement_write_scratch)?;
        File::open(&paths.root)?.sync_all()?;
    }
    let active = read_manifest(&paths.manifest)?;
    let mut candidate_exists = path_exists(&paths.replacement_candidate)?;
    let candidate_next_exists = path_exists(&paths.replacement_candidate_next)?;
    let mut submission_exists = path_exists(&paths.replacement_submission)?;
    let submission_next_exists = path_exists(&paths.replacement_submission_next)?;
    let mut promotion_exists = path_exists(&paths.replacement_promotion)?;
    let promotion_next_exists = path_exists(&paths.replacement_promotion_next)?;
    let next_exists = path_exists(&paths.next_manifest)?;

    if candidate_next_exists {
        let candidate_next = read_replacement_candidate(&paths.replacement_candidate_next)?;
        if candidate_exists {
            if submission_exists || submission_next_exists {
                return Err(TransportError::Codec(
                    "candidate refresh journal exists after replacement submission".into(),
                ));
            }
            let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
            validate_candidate_refresh_pair(&candidate, &candidate_next)?;
            remove_file_if_exists(&paths.replacement_candidate_next)?;
        } else {
            if submission_exists || submission_next_exists || next_exists {
                return Err(TransportError::Codec(
                    "initial candidate journal conflicts with later replacement state".into(),
                ));
            }
            validate_replacement_candidate_state(&candidate_next, &active, node_host)?;
            fs::rename(
                &paths.replacement_candidate_next,
                &paths.replacement_candidate,
            )?;
            candidate_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }

    if submission_next_exists {
        if !candidate_exists {
            return Err(TransportError::Codec(
                "replacement submission journal is missing its candidate".into(),
            ));
        }
        let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
        let submission_next = read_replacement_submission(&paths.replacement_submission_next)?;
        validate_durable_replacement_submission(&candidate.manifest, &submission_next)?;
        if submission_exists {
            if read_replacement_submission(&paths.replacement_submission)? != submission_next {
                return Err(TransportError::Codec(
                    "replacement submission journal conflicts with durable state".into(),
                ));
            }
            remove_file_if_exists(&paths.replacement_submission_next)?;
        } else {
            fs::rename(
                &paths.replacement_submission_next,
                &paths.replacement_submission,
            )?;
            submission_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }

    if promotion_next_exists {
        if !candidate_exists || !submission_exists {
            return Err(TransportError::Codec(
                "replacement promotion journal is missing candidate submission state".into(),
            ));
        }
        let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
        let submission = read_replacement_submission(&paths.replacement_submission)?;
        let expected = replacement_authorization(&candidate, &submission)?;
        if read_replacement_promotion(&paths.replacement_promotion_next)? != expected {
            return Err(TransportError::Codec(
                "replacement promotion journal targets another authorization".into(),
            ));
        }
        fs::rename(
            &paths.replacement_promotion_next,
            &paths.replacement_promotion,
        )?;
        promotion_exists = true;
        File::open(&paths.root)?.sync_all()?;
    }

    let active_hash = active.authorization_hash().map_err(codec_error)?;
    if !candidate_exists {
        if next_exists {
            return Err(TransportError::Codec(
                "replacement journal is missing its candidate record".into(),
            ));
        }
        let durable_promotion = if promotion_exists {
            Some(read_replacement_promotion(&paths.replacement_promotion)?)
        } else {
            None
        };
        if submission_exists {
            let promotion = durable_promotion.ok_or_else(|| {
                TransportError::Codec(
                    "replacement submission residue is missing its promotion receipt".into(),
                )
            })?;
            let submission = read_replacement_submission(&paths.replacement_submission)?;
            let intent = validate_durable_replacement_submission(&active, &submission)?;
            if promotion.candidate_manifest_hash != active_hash
                || intent.intent_hash().map_err(codec_error)? != promotion.intent_hash
            {
                return Err(TransportError::Codec(
                    "replacement submission residue conflicts with committed promotion".into(),
                ));
            }
            remove_file_if_exists(&paths.replacement_submission)?;
            File::open(&paths.root)?.sync_all()?;
            submission_exists = false;
        }
        if submission_exists {
            return Err(TransportError::Codec(
                "replacement submission residue could not be reconciled".into(),
            ));
        }
        if durable_promotion
            .is_some_and(|promotion| promotion.candidate_manifest_hash != active_hash)
        {
            return Err(TransportError::Codec(
                "replacement promotion receipt does not target the active manifest".into(),
            ));
        }
        return Ok(());
    }

    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let candidate_hash = candidate
        .manifest
        .authorization_hash()
        .map_err(codec_error)?;
    let submission_authorization = if submission_exists {
        Some(replacement_authorization(
            &candidate,
            &read_replacement_submission(&paths.replacement_submission)?,
        )?)
    } else {
        None
    };
    let durable_promotion = if promotion_exists {
        Some(read_replacement_promotion(&paths.replacement_promotion)?)
    } else {
        None
    };
    if active_hash == candidate.predecessor_manifest_hash {
        validate_replacement_candidate_state(&candidate, &active, node_host)?;
        if let Some(promotion) = durable_promotion {
            let prior_active_receipt = promotion.candidate_manifest_hash == active_hash;
            if (!prior_active_receipt || next_exists) && submission_authorization != Some(promotion)
            {
                return Err(TransportError::Codec(
                    "replacement promotion receipt conflicts with staged authorization".into(),
                ));
            }
        }
        if next_exists {
            if !submission_exists || durable_promotion != submission_authorization {
                return Err(TransportError::Codec(
                    "next replacement manifest exists without exact durable promotion state".into(),
                ));
            }
            let expected = candidate.manifest.encode_canonical().map_err(codec_error)?;
            if read_owned_bounded_file(
                &paths.next_manifest,
                MAX_INITIALIZATION_MANIFEST_BYTES,
                "next replacement manifest",
            )? != expected
            {
                return Err(TransportError::Codec(
                    "next replacement manifest conflicts with the staged candidate".into(),
                ));
            }
        }
        return Ok(());
    }
    if active_hash == candidate_hash && active == candidate.manifest {
        if active.node_host_noise_x25519 != node_host.public() {
            return Err(TransportError::Codec(
                "promoted manifest does not match the persistent NodeHost key".into(),
            ));
        }
        let promotion = durable_promotion.ok_or_else(|| {
            TransportError::Codec("promoted manifest is missing its authorization receipt".into())
        })?;
        if promotion.candidate_manifest_hash != active_hash
            || submission_authorization.is_some_and(|expected| expected != promotion)
        {
            return Err(TransportError::Codec(
                "promoted manifest authorization receipt is inconsistent".into(),
            ));
        }
        if next_exists {
            let expected = active.encode_canonical().map_err(codec_error)?;
            if read_owned_bounded_file(
                &paths.next_manifest,
                MAX_INITIALIZATION_MANIFEST_BYTES,
                "next replacement manifest",
            )? != expected
            {
                return Err(TransportError::Codec(
                    "post-promotion next manifest conflicts with committed state".into(),
                ));
            }
            remove_file_if_exists(&paths.next_manifest)?;
        }
        remove_file_if_exists(&paths.replacement_submission)?;
        File::open(&paths.root)?.sync_all()?;
        remove_file_if_exists(&paths.replacement_candidate)?;
        File::open(&paths.root)?.sync_all()?;
        return Ok(());
    }
    Err(TransportError::Codec(
        "replacement journal does not descend from or equal committed state".into(),
    ))
}

fn promote_pending_manifest(paths: &NodeHostPaths) -> Result<(), TransportError> {
    if path_exists(&paths.manifest)? {
        return Err(TransportError::Codec(
            "refusing to replace a committed NodeHost manifest".into(),
        ));
    }
    fs::rename(&paths.pending_manifest, &paths.manifest)?;
    File::open(&paths.root)?.sync_all()?;
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), TransportError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir()
                || metadata.permissions().mode() & 0o777 != 0o700
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(TransportError::Codec(
                    "NodeHost state path must be a current-user directory with mode 0700".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)?;
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool, TransportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

struct NodeHostPaths {
    root: PathBuf,
    state_lock: PathBuf,
    noise_key: PathBuf,
    manifest: PathBuf,
    pending_manifest: PathBuf,
    replacement_candidate: PathBuf,
    replacement_submission: PathBuf,
    replacement_promotion: PathBuf,
    next_manifest: PathBuf,
    replacement_candidate_next: PathBuf,
    replacement_submission_next: PathBuf,
    replacement_promotion_next: PathBuf,
    replacement_write_scratch: PathBuf,
}

impl NodeHostPaths {
    fn new(node_data_dir: &Path) -> Self {
        let root = node_data_dir.join(NODE_HOST_DIRECTORY_V1);
        Self {
            state_lock: root.join(NODE_HOST_STATE_LOCK_V1),
            noise_key: root.join(NODE_HOST_NOISE_KEY_V1),
            manifest: root.join(NODE_HOST_MANIFEST_V1),
            pending_manifest: root.join(NODE_HOST_PENDING_MANIFEST_V1),
            replacement_candidate: root.join(NODE_HOST_REPLACEMENT_CANDIDATE_V1),
            replacement_submission: root.join(NODE_HOST_REPLACEMENT_SUBMISSION_V1),
            replacement_promotion: root.join(NODE_HOST_REPLACEMENT_PROMOTION_V1),
            next_manifest: root.join(NODE_HOST_NEXT_MANIFEST_V1),
            replacement_candidate_next: root.join(NODE_HOST_REPLACEMENT_CANDIDATE_NEXT_V1),
            replacement_submission_next: root.join(NODE_HOST_REPLACEMENT_SUBMISSION_NEXT_V1),
            replacement_promotion_next: root.join(NODE_HOST_REPLACEMENT_PROMOTION_NEXT_V1),
            replacement_write_scratch: root.join(NODE_HOST_REPLACEMENT_WRITE_SCRATCH_V1),
            root,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use k256::ecdsa::signature::hazmat::PrehashSigner as _;
    use outbe_primitives::tee_attestation_v1::{
        AttestationMode, DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
        TransitionKeyReadyProofV1,
    };

    struct ReplacementFixture {
        _root: tempfile::TempDir,
        node_data_dir: PathBuf,
        paths: NodeHostPaths,
        node_host_public: [u8; 32],
        active: EnclaveInitializationManifestV1,
        candidate: EnclaveInitializationManifestV1,
        authorization: FinalizedReplacementAuthorizationV1,
    }

    fn replacement_fixture() -> ReplacementFixture {
        replacement_fixture_for_operation(AttestationOperationV1::ReplaceEnclaveBinding)
    }

    fn replacement_fixture_for_operation(operation: AttestationOperationV1) -> ReplacementFixture {
        let root = tempfile::tempdir().unwrap();
        let node_data_dir = root.path().join("node-data");
        std::fs::create_dir(&node_data_dir).unwrap();
        let paths = NodeHostPaths::new(&node_data_dir);
        ensure_private_directory(&paths.root).unwrap();
        let node_host = NodeHostNoiseKey::create_new(&paths.noise_key).unwrap();
        let node_host_public = node_host.public();
        let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
        let reth_p2p_public = node_signer
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x62; 32]);
        let node_id = NodeIdV1 { reth_p2p_public };
        let active = EnclaveInitializationManifestV1 {
            chain_id: U256::from(19_u64).to_be_bytes(),
            genesis_hash: B256::repeat_byte(0x14),
            node_id: node_id.clone(),
            initialization_challenge: [0x41; 32],
            node_host_noise_x25519: node_host.public(),
            recipient_x25519: [0x51; 32],
            attestation_ed25519: [0x52; 32],
            noise_responder_x25519: [0x53; 32],
        };
        let candidate = EnclaveInitializationManifestV1 {
            initialization_challenge: [0x42; 32],
            recipient_x25519: [0x61; 32],
            attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
            noise_responder_x25519: [0x63; 32],
            ..active.clone()
        };
        write_manifest_once(&paths.manifest, &active, &paths.root).unwrap();
        let record = ReplacementCandidateRecordV1 {
            predecessor_manifest_hash: active.authorization_hash().unwrap(),
            manifest: candidate.clone(),
        };
        write_bytes_once(
            &paths.replacement_candidate,
            &record.encode_canonical().unwrap(),
            &paths.root,
        )
        .unwrap();

        let intent = RegistrationIntentV1 {
            chain_id: candidate.chain_id,
            genesis_hash: candidate.genesis_hash,
            operation,
            attestation_mode: AttestationMode::DcapRequired,
            policy_hash: B256::repeat_byte(0x21),
            node_id,
            enclave_id: candidate.enclave_id().unwrap(),
            binding_id: B256::repeat_byte(0x45),
            binding_version: 2,
            registration_version: 1,
            renewal_nonce: 0,
            transition_nonce: u64::from(
                operation == AttestationOperationV1::TransitionEnclaveMeasurement,
            ),
            requested_valid_until: 20_000,
            recipient_x25519: candidate.recipient_x25519,
            attestation_ed25519: candidate.attestation_ed25519,
            noise_responder_x25519: candidate.noise_responder_x25519,
            node_host_authorization_hash: candidate.node_host_authorization_hash().unwrap(),
        };
        let intent_hash = intent.intent_hash().unwrap();
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
            node_signer.sign_prehash(intent_hash.as_slice()).unwrap();
        let mut node_signature = [0_u8; 65];
        node_signature[..64].copy_from_slice(signature.to_bytes().as_slice());
        node_signature[64] = recovery.to_byte();
        let enclave_signature = enclave_signer.sign(intent_hash.as_slice()).to_bytes();
        let transition_key_ready_proof =
            (operation == AttestationOperationV1::TransitionEnclaveMeasurement).then(|| {
                let mut proof = TransitionKeyReadyProofV1 {
                    chain_id: intent.chain_id,
                    genesis_hash: intent.genesis_hash,
                    transition_intent_hash: intent_hash,
                    candidate_manifest_hash: candidate.authorization_hash().unwrap(),
                    transition_nonce: intent.transition_nonce,
                    resident_offer_public: [0x71; 32],
                    candidate_attestation_signature: [0; 64],
                };
                proof.candidate_attestation_signature = enclave_signer
                    .sign(proof.signing_hash().unwrap().as_slice())
                    .to_bytes();
                proof
            });
        let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent,
            quote: vec![0x51],
            components: (1_u8..=8)
                .map(|kind| DcapCollateralComponentV1 {
                    kind: DcapCollateralKind::try_from(kind).unwrap(),
                    bytes: vec![kind],
                })
                .collect(),
            transition_key_ready_proof,
        });
        persist_replacement_candidate_submission(
            &node_data_dir,
            &evidence,
            &node_signature,
            &enclave_signature,
        )
        .unwrap();
        let candidate_hash = candidate.authorization_hash().unwrap();
        let authorization =
            FinalizedReplacementAuthorizationV1::for_test(intent_hash, candidate_hash);
        ReplacementFixture {
            _root: root,
            node_data_dir,
            paths,
            node_host_public,
            active,
            candidate,
            authorization,
        }
    }

    #[test]
    fn measurement_transition_reuses_the_finalized_candidate_workflow() {
        let fixture =
            replacement_fixture_for_operation(AttestationOperationV1::TransitionEnclaveMeasurement);
        let submission = load_replacement_candidate_submission(&fixture.node_data_dir)
            .unwrap()
            .unwrap();
        let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap();
        let AttestationEvidenceV1::Dcap(evidence) = evidence else {
            panic!("expected DCAP transition evidence")
        };
        assert_eq!(
            evidence.intent.operation,
            AttestationOperationV1::TransitionEnclaveMeasurement
        );
        assert_eq!(evidence.intent.transition_nonce, 1);
        assert_eq!(
            promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
            fixture.candidate
        );
    }

    #[test]
    fn finalized_exact_candidate_promotes_atomically_and_idempotently() {
        let fixture = replacement_fixture();
        assert_ne!(fixture.active, fixture.candidate);

        assert_eq!(
            promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
            fixture.candidate
        );
        let wrong_completed_intent = FinalizedReplacementAuthorizationV1::for_test(
            B256::repeat_byte(0x93),
            fixture.authorization.candidate_manifest_hash,
        );
        assert!(
            promote_replacement_candidate(&fixture.node_data_dir, &wrong_completed_intent)
                .unwrap_err()
                .to_string()
                .contains("completed promotion authorization does not match")
        );
        assert_eq!(
            promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
            fixture.candidate
        );
        assert!(!fixture.paths.replacement_candidate.exists());
        assert!(!fixture.paths.replacement_submission.exists());
        assert_eq!(
            NodeHostNoiseKey::load(&fixture.paths.noise_key)
                .unwrap()
                .public(),
            fixture.node_host_public
        );
        assert_eq!(
            read_manifest(&fixture.paths.manifest).unwrap(),
            fixture.candidate
        );
    }

    #[test]
    fn production_authority_is_constructed_only_from_the_exact_finalized_replacement_binding() {
        let fixture = replacement_fixture();
        let candidate = read_replacement_candidate(&fixture.paths.replacement_candidate).unwrap();
        let submission =
            read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
        let intent =
            validate_durable_replacement_submission(&candidate.manifest, &submission).unwrap();
        let finalized = FinalizedReplacementBindingV1 {
            view: crate::FinalizedRegistryViewV1 {
                chain_id: intent.chain_id,
                genesis_hash: intent.genesis_hash,
                block_number: 90,
                block_hash: B256::repeat_byte(0xA1),
                state_root: B256::repeat_byte(0xA2),
                consensus_timestamp: 19_000,
            },
            node_id_hash: intent.node_id.node_id_hash().unwrap(),
            enclave_id: intent.enclave_id,
            binding_id: intent.binding_id,
            intent_hash: intent.intent_hash().unwrap(),
            binding_version: intent.binding_version,
            registration_version: intent.registration_version,
            valid_until: 20_000,
            recipient_x25519: intent.recipient_x25519,
            attestation_ed25519: intent.attestation_ed25519,
            noise_responder_x25519: intent.noise_responder_x25519,
            node_host_authorization_hash: intent.node_host_authorization_hash,
        };

        assert_eq!(
            construct_finalized_replacement_authorization_v1(&fixture.node_data_dir, &finalized,)
                .unwrap(),
            fixture.authorization
        );

        let mut wrong = finalized;
        wrong.intent_hash = B256::repeat_byte(0xA3);
        assert!(
            construct_finalized_replacement_authorization_v1(&fixture.node_data_dir, &wrong,)
                .unwrap_err()
                .to_string()
                .contains("finalized Registry binding does not match")
        );

        let mut wrong_lease = finalized;
        wrong_lease.valid_until -= 1;
        assert!(construct_finalized_replacement_authorization_v1(
            &fixture.node_data_dir,
            &wrong_lease,
        )
        .unwrap_err()
        .to_string()
        .contains("finalized Registry binding does not match"));
    }

    #[test]
    fn promotion_requires_the_exact_finalized_intent_and_candidate() {
        let fixture = replacement_fixture();
        let wrong_candidate = FinalizedReplacementAuthorizationV1::for_test(
            fixture.authorization.intent_hash,
            B256::repeat_byte(0x91),
        );
        assert!(
            promote_replacement_candidate(&fixture.node_data_dir, &wrong_candidate)
                .unwrap_err()
                .to_string()
                .contains("targets another candidate manifest")
        );

        let wrong_intent = FinalizedReplacementAuthorizationV1::for_test(
            B256::repeat_byte(0x92),
            fixture.authorization.candidate_manifest_hash,
        );
        assert!(
            promote_replacement_candidate(&fixture.node_data_dir, &wrong_intent)
                .unwrap_err()
                .to_string()
                .contains("targets another replacement intent")
        );

        assert_eq!(
            read_manifest(&fixture.paths.manifest).unwrap(),
            fixture.active
        );
        assert!(fixture.paths.replacement_candidate.exists());
        assert!(fixture.paths.replacement_submission.exists());
        assert!(!fixture.paths.next_manifest.exists());
    }

    #[test]
    fn restart_rejects_a_candidate_refresh_that_changes_enclave_identity() {
        let fixture = replacement_fixture();
        std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
        File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
        let mut candidate =
            read_replacement_candidate(&fixture.paths.replacement_candidate).unwrap();
        candidate.manifest.recipient_x25519[0] ^= 1;
        write_bytes_once(
            &fixture.paths.replacement_candidate_next,
            &candidate.encode_canonical().unwrap(),
            &fixture.paths.root,
        )
        .unwrap();

        let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
        assert!(reconcile_replacement_state(&fixture.paths, &node_host)
            .unwrap_err()
            .to_string()
            .contains("changes replacement identity"));
        assert_eq!(
            read_manifest(&fixture.paths.manifest).unwrap(),
            fixture.active
        );
        assert!(fixture.paths.replacement_candidate_next.exists());
    }

    #[test]
    fn restart_atomically_recovers_candidate_and_submission_and_reloads_exact_bytes() {
        let fixture = replacement_fixture();
        let candidate_record = std::fs::read(&fixture.paths.replacement_candidate).unwrap();
        let submission_record = std::fs::read(&fixture.paths.replacement_submission).unwrap();
        let expected_submission =
            read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
        std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
        std::fs::remove_file(&fixture.paths.replacement_candidate).unwrap();
        File::open(&fixture.paths.root).unwrap().sync_all().unwrap();

        write_bytes_once(
            &fixture.paths.replacement_candidate_next,
            &candidate_record,
            &fixture.paths.root,
        )
        .unwrap();
        let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
        reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
        assert!(fixture.paths.replacement_candidate.exists());
        assert!(!fixture.paths.replacement_candidate_next.exists());

        write_bytes_once(
            &fixture.paths.replacement_submission_next,
            &submission_record,
            &fixture.paths.root,
        )
        .unwrap();
        assert_eq!(
            load_replacement_candidate_submission(&fixture.node_data_dir).unwrap(),
            Some(expected_submission)
        );
        assert!(fixture.paths.replacement_submission.exists());
        assert!(!fixture.paths.replacement_submission_next.exists());
    }

    #[test]
    fn restart_never_auto_promotes_and_cleans_an_already_committed_candidate() {
        let fixture = replacement_fixture();
        let candidate_record = std::fs::read(&fixture.paths.replacement_candidate).unwrap();
        let submission_record = std::fs::read(&fixture.paths.replacement_submission).unwrap();
        std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
        File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
        write_bytes_once(
            &fixture.paths.replacement_candidate_next,
            &candidate_record,
            &fixture.paths.root,
        )
        .unwrap();
        let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
        reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
        assert!(!fixture.paths.replacement_candidate_next.exists());
        write_bytes_once(
            &fixture.paths.replacement_submission,
            &submission_record,
            &fixture.paths.root,
        )
        .unwrap();

        write_bytes_once(
            &fixture.paths.replacement_promotion,
            &fixture.authorization.encode_canonical(),
            &fixture.paths.root,
        )
        .unwrap();
        let candidate_bytes = fixture.candidate.encode_canonical().unwrap();
        write_bytes_once(
            &fixture.paths.next_manifest,
            &candidate_bytes,
            &fixture.paths.root,
        )
        .unwrap();
        reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
        assert_eq!(
            read_manifest(&fixture.paths.manifest).unwrap(),
            fixture.active
        );
        assert!(fixture.paths.next_manifest.exists());
        assert!(fixture.paths.replacement_candidate.exists());
        assert!(fixture.paths.replacement_submission.exists());

        fs::rename(&fixture.paths.next_manifest, &fixture.paths.manifest).unwrap();
        File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
        reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
        assert_eq!(
            read_manifest(&fixture.paths.manifest).unwrap(),
            fixture.candidate
        );
        assert!(!fixture.paths.next_manifest.exists());
        assert!(!fixture.paths.replacement_candidate.exists());
        assert!(!fixture.paths.replacement_submission.exists());
        assert!(fixture.paths.replacement_promotion.exists());
    }

    #[test]
    fn replacement_state_lock_serializes_first_submission_writer() {
        use std::{sync::mpsc, time::Duration};

        use rustix::fs::FlockOperation;

        let fixture = replacement_fixture();
        let submission =
            read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
        let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap();
        let node_signature = *submission.node_signature();
        let enclave_signature = *submission.enclave_signature();
        std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
        File::open(&fixture.paths.root).unwrap().sync_all().unwrap();

        let lock_path = fixture.paths.root.join("state.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)
            .unwrap();
        rustix::fs::flock(&lock, FlockOperation::LockExclusive).unwrap();

        let node_data_dir = fixture.node_data_dir.clone();
        let (result_sender, result_receiver) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let result = persist_replacement_candidate_submission(
                &node_data_dir,
                &evidence,
                &node_signature,
                &enclave_signature,
            );
            result_sender.send(result).unwrap();
        });
        assert!(
            result_receiver
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "submission writer bypassed the NodeHost state lock"
        );
        rustix::fs::flock(&lock, FlockOperation::Unlock).unwrap();
        drop(lock);
        assert!(result_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_ok());
        writer.join().unwrap();
    }

    #[test]
    fn restart_discards_an_uncommitted_torn_replacement_scratch() {
        let fixture = replacement_fixture();
        let scratch = fixture.paths.root.join("replacement-write.tmp");
        write_bytes_once(&scratch, b"torn", &fixture.paths.root).unwrap();

        assert!(
            load_replacement_candidate_submission(&fixture.node_data_dir)
                .unwrap()
                .is_some()
        );
        assert!(!scratch.exists());
        assert_eq!(
            read_manifest(&fixture.paths.manifest).unwrap(),
            fixture.active
        );
    }

    #[test]
    fn restart_recovers_submission_only_residue_after_committed_promotion() {
        let fixture = replacement_fixture();
        write_bytes_once(
            &fixture.paths.replacement_promotion,
            &fixture.authorization.encode_canonical(),
            &fixture.paths.root,
        )
        .unwrap();
        write_bytes_once(
            &fixture.paths.next_manifest,
            &fixture.candidate.encode_canonical().unwrap(),
            &fixture.paths.root,
        )
        .unwrap();
        fs::rename(&fixture.paths.next_manifest, &fixture.paths.manifest).unwrap();
        std::fs::remove_file(&fixture.paths.replacement_candidate).unwrap();
        File::open(&fixture.paths.root).unwrap().sync_all().unwrap();

        let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
        reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
        assert_eq!(
            read_manifest(&fixture.paths.manifest).unwrap(),
            fixture.candidate
        );
        assert!(!fixture.paths.replacement_submission.exists());
        assert!(fixture.paths.replacement_promotion.exists());
    }
}
