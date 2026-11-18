//! Blockchain-layer TEE infrastructure and resident cryptographic operations.
//!
//! Process secrets and derived slot keys live only in RAM inside `outbe-tee`,
//! behind opaque handles. Resident state is never persisted to disk. Domain
//! adapters supply byte labels and contexts; this crate has no dependencies on
//! `core/` or `system/` and contains no application protocol rules.
//!
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
pub use outbe_dcap::dcap_protocol;
pub use outbe_dcap::dcap_v1;
pub mod errors;
pub mod finalized_admission;
pub mod host_collateral;
pub use outbe_dcap::verifier;
pub mod journal_storage;
pub mod lifecycle;
pub mod metrics;
#[cfg(feature = "native-dcap")]
pub use outbe_dcap::native_qvl;
pub mod node_host;
pub mod protocol;
pub use outbe_dcap::quote;
pub mod registry_view;
pub use outbe_dcap::release_dcap_artifacts;
pub mod remote_session;
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
pub use witness::{consensus_call, TypedWitnessPayload, TypedWitnessPayloadRef};

pub use canary::{TeeEnclaveHealthChannel, TeeEnclaveHealthSnapshot, TeeEnclaveHealthState};
pub use dcap_protocol::RegistrationVerificationRequest;
pub use dcap_v1::{dcap_collateral_validity_window_v1, DcapCollateralValidityWindowV1};
pub use errors::{
    AppErrorDescriptor, CallContextError, CallContextProviderError, DcapFixtureError,
    QuoteParseError, TeeCryptoError, TransportError,
};
pub use host_collateral::acquire_dcap_collateral_v1;
pub use in_process::GeneratedDcapQuoteV1;
#[cfg(any(test, feature = "test-utils"))]
pub use in_process_dispatch::{
    canary_attestation_pub, canary_request_with_meta, try_account_query,
};
pub use in_process_dispatch::{
    in_process_tee_context, install_in_process_tee_context, is_enclave_configured,
    prepare_no_attestation_dev_onboarding_artifact_v1, replace_in_process_tee_context,
    resident_network_encryption_public_key_state_v1, resident_network_encryption_public_key_v1,
    try_account_query_with_meta, try_in_process_request, verify_dcap_evidence_v1,
    verify_dcap_registration_and_seal_v1, InProcessDispatchError, InProcessPayloadHandler,
    InstallError,
};
pub use node_host::{
    construct_finalized_replacement_authorization_v1, load_replacement_candidate_relay,
    load_replacement_candidate_submission, persist_replacement_candidate_relay,
    persist_replacement_candidate_submission, promote_replacement_candidate, CommittedJoinRelayV1,
    CommittedJoinSubmissionV1, FinalizedJoinAdmissionAnchorV1, FinalizedReplacementAuthorizationV1,
    FinalizedReplacementBindingV1, NodeHostIdentityV1, ReplacementCandidateRelayV1,
    ReplacementCandidateSubmissionV1,
};
pub use remote_session::{
    admit_remote_session_v1, admit_rpc_trusted_remote_session_v1, FinalizedRegistryBindingV1,
    FinalizedRegistryViewV1, RemoteSessionAdmissionError, RemoteSessionAdmissionV1,
    RemoteSessionExpectationV1, RpcTrustedRemoteSessionV1,
};
pub use tee_call::{
    tee_consensus_call, tee_consensus_typed_call, tee_session_call, tee_session_typed_call,
    verify_signed_rejection, CallMeta, SignedRejectionRef, TeeAttestationError,
};
pub use tee_dkg::{CeremonyCoordinator, CeremonyOutcome, EnclaveChannel};
