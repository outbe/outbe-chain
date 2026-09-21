//! Persistent host-side authorization for one production node enclave.
//!
//! The private NodeHost Noise key is write-once. A canonical public manifest is
//! first written as `pending`, committed by the enclave, then promoted to the
//! restart record. A crash after enclave commit but before promotion is closed
//! by reconnecting with the pending record and promoting it only on success.

use crate::TransportError;

impl FinalizedReplacementAuthorizationV1 {}

fn codec_error(error: outbe_primitives::tee_attestation_v1::CodecError) -> TransportError {
    TransportError::Codec(error.to_string())
}

#[cfg(test)]
mod tests;

mod filesystem;
use filesystem::{
    ensure_private_directory, path_exists, read_owned_bounded_file, remove_file_if_exists,
    replace_bytes_atomically, write_bytes_once_or_exact, write_manifest_once, NodeHostPaths,
    NodeHostStateLock,
};
pub use filesystem::{
    NODE_HOST_COMMITTED_JOIN_RELAY_V1, NODE_HOST_COMMITTED_JOIN_SUBMISSION_V1,
    NODE_HOST_DIRECTORY_V1, NODE_HOST_FINALIZED_JOIN_ADMISSION_ANCHOR_V1, NODE_HOST_MANIFEST_V1,
    NODE_HOST_NOISE_KEY_V1, NODE_HOST_REPLACEMENT_CANDIDATE_V1, NODE_HOST_REPLACEMENT_PROMOTION_V1,
    NODE_HOST_REPLACEMENT_RELAY_V1, NODE_HOST_REPLACEMENT_SUBMISSION_V1,
};

#[cfg(test)]
use filesystem::write_bytes_once;

mod join_records;
use join_records::{
    read_committed_join_relay, read_committed_join_submission, read_finalized_join_admission_anchor,
};
pub use join_records::{
    CommittedJoinRelayV1, CommittedJoinSubmissionV1, FinalizedJoinAdmissionAnchorV1,
};

mod replacement_records;
use replacement_records::{
    read_replacement_candidate, read_replacement_promotion, read_replacement_relay,
    read_replacement_submission, ReplacementCandidateRecordV1, MAX_REPLACEMENT_SUBMISSION_BYTES,
};
pub use replacement_records::{
    FinalizedReplacementAuthorizationV1, FinalizedReplacementBindingV1,
    ReplacementCandidateRelayV1, ReplacementCandidateSubmissionV1,
};

mod identity;
pub use identity::{
    committed_node_host_session_material, connect_committed_node_host_enclave,
    connect_or_initialize_node_host_enclave, load_committed_enclave_manifest_v1,
    NodeHostIdentityV1,
};
use identity::{
    read_manifest, sign_manifest, validate_identity, validate_manifest_identity,
    MAX_INITIALIZATION_MANIFEST_BYTES,
};

mod committed_join;
pub use committed_join::{
    clear_committed_join_checkpoint, load_committed_join_relay, load_committed_join_submission,
    load_finalized_join_admission_anchor, persist_committed_join_relay,
    persist_committed_join_submission, persist_finalized_join_admission_anchor,
};
use committed_join::{
    validate_anchor_replacement, validate_durable_committed_join_submission,
    validate_finalized_join_admission_anchor,
};

mod replacement;
pub use replacement::{
    construct_finalized_replacement_authorization_v1, load_replacement_candidate_relay,
    load_replacement_candidate_submission, persist_replacement_candidate_relay,
    persist_replacement_candidate_submission, prepare_node_host_enclave_replacement_candidate,
    promote_replacement_candidate, ReplacementCandidateEnclaveV1,
};
use replacement::{validate_durable_replacement_submission, validate_replacement_candidate_state};

mod recovery;
use recovery::{
    reconcile_committed_join_state, reconcile_finalized_join_admission_anchor,
    reconcile_replacement_state, replacement_authorization,
};
