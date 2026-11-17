//! Blockchain-layer TEE infrastructure and resident cryptographic operations.
//!
//! Process secrets and derived slot keys live only in RAM inside `outbe-tee`,
//! behind opaque handles. Resident state is never persisted to disk. Domain
//! adapters supply byte labels and contexts; this crate has no dependencies on
//! `core/` or `system/` and contains no application protocol rules.
//!
//! Legacy transport, attestation, and witness APIs remain available until their
//! callers switch to the in-process context.

pub mod authority;
#[cfg(any(test, feature = "test-utils"))]
pub use authority::{
    clear_tee_authority_watch_for_test, tee_authority_test_guard, TeeAuthorityTestGuard,
};
pub use authority::{
    current_tee_authority_state, install_tee_authority_watch, tee_authority_watch,
    TeeAuthorityChannel, TeeAuthorityInstallError, TeeAuthorityState, TeeAuthorityWatch,
};

pub mod in_process;
pub mod in_process_dispatch;
pub mod resident;
use outbe_dcap::byte_cursor;
pub mod call_context;
pub mod canary;
pub mod client;
pub mod client_global;
pub mod codec;
pub use outbe_dcap::dcap_protocol;
pub use outbe_dcap::dcap_v1;
pub mod errors;
pub mod finalized_admission;
pub mod host_collateral;
pub use outbe_dcap::verifier;
pub mod journal_storage;
pub mod metrics;
#[cfg(feature = "native-dcap")]
pub use outbe_dcap::native_qvl;
pub mod node_host;
pub mod protocol;
pub use outbe_dcap::quote;
pub mod registry_view;
pub use outbe_dcap::release_dcap_artifacts;
pub mod remote_session;
pub mod session;
pub mod tee_attestation_activation;
pub mod tee_call;
pub mod tee_decrypt;
pub mod tee_dkg;
pub mod tee_encrypt;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
pub mod upgrade_journal;
pub mod upgrade_transfer;
pub mod witness;
pub use witness::consensus_call;

pub use canary::{TeeEnclaveHealthChannel, TeeEnclaveHealthSnapshot, TeeEnclaveHealthState};
pub use client::{
    verify_peer_quote, AttestedPeerKeys, AuthorizedEnclaveClient, EnclaveClient,
    EnclaveInitializationChallenge, GeneratedDcapQuoteV1, NodeHostNoiseKey, RemoteEnclaveClient,
    RemoteEnclavePublicKeysV1, RemoteSessionTicketV1,
};
pub use client_global::{
    canary_attestation_pub, canary_request, canary_request_with_meta, generate_dcap_quote_v1,
    install_authorized_enclave_client, install_enclave_client, is_enclave_configured,
    prepare_no_attestation_dev_onboarding_artifact_v1,
    resident_network_encryption_public_key_state_v1, resident_network_encryption_public_key_v1,
    try_account_query, try_account_query_with_meta, try_with_enclave, verify_dcap_evidence_v1,
    verify_dcap_registration_and_seal_v1, InstallError, RegistrationVerificationRequest,
    RuntimeEnclaveClient,
};
pub use dcap_v1::{dcap_collateral_validity_window_v1, DcapCollateralValidityWindowV1};
pub use errors::{
    AppErrorDescriptor, CallContextError, CallContextProviderError, DcapFixtureError,
    DevIdentityMismatchError, QuoteParseError, TeeCryptoError, TransportError,
};
pub use host_collateral::acquire_dcap_collateral_v1;
pub use in_process_dispatch::{
    in_process_tee_context, install_in_process_tee_context, try_in_process_request,
    InProcessDispatchError, InProcessPayloadHandler,
};
pub use node_host::{
    clear_committed_join_checkpoint, connect_committed_node_host_enclave,
    connect_or_initialize_node_host_enclave, construct_finalized_replacement_authorization_v1,
    load_committed_enclave_manifest_v1, load_committed_join_relay, load_committed_join_submission,
    load_finalized_join_admission_anchor, load_replacement_candidate_relay,
    load_replacement_candidate_submission, persist_committed_join_relay,
    persist_committed_join_submission, persist_finalized_join_admission_anchor,
    persist_replacement_candidate_relay, persist_replacement_candidate_submission,
    prepare_node_host_enclave_replacement_candidate, promote_replacement_candidate,
    CommittedJoinRelayV1, CommittedJoinSubmissionV1, FinalizedAdmissionBeginInputV1,
    FinalizedAdmissionIngestInputV1, FinalizedJoinAdmissionAnchorV1,
    FinalizedReplacementAuthorizationV1, FinalizedReplacementBindingV1, NodeHostIdentityV1,
    ReplacementCandidateEnclaveV1, ReplacementCandidateRelayV1, ReplacementCandidateSubmissionV1,
};
pub use remote_session::{
    admit_remote_session_v1, admit_rpc_trusted_remote_session_v1, FinalizedRegistryBindingV1,
    FinalizedRegistryViewV1, RemoteSessionAdmissionError, RemoteSessionAdmissionV1,
    RemoteSessionExpectationV1, RpcTrustedRemoteSessionV1,
};
pub use session::EnclaveSession;
pub use tee_call::{
    tee_consensus_call, tee_session_call, tee_session_call_on, verify_signed_rejection, CallMeta,
    SignedRejectionRef, TeeAttestationError,
};
pub use tee_dkg::{CeremonyCoordinator, CeremonyOutcome, EnclaveChannel};

/// Noise pattern for the node <-> enclave channel: **IK** (the responder/enclave
/// static key is known to the initiator/host via the attested quote), with
/// X25519 + ChaChaPoly + SHA256.
pub const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// Version of the enclave's durable, network-bound sealed-state payload.
/// Release metadata and the enclave decoder use this single constant.
pub const SEALED_STATE_SCHEMA_V1: u8 = 3;

/// Magic prefix of a `TSEAL` sealed-state blob.
pub const SEAL_MAGIC: &[u8; 5] = b"TSEAL";

/// Magic prefix of a combined `MRENCLAVE | MRSIGNER` hardware-sealed `TSGX1` blob.
pub const SGX_COMBINED_SEAL_MAGIC: &[u8; 5] = b"TSGX1";

/// Encoded `TSEAL` header byte length (excluding magic): `1 + 1 + 2 + 8 + 8 + 12`.
pub const SEAL_HEADER_LEN: usize = 32;
