//! Enclave-side transport: framed UDS server + Noise-IK responder + dispatch.
//!
//! Production accepts only initialization discovery, a signed write-once
//! initialization manifest, or `OpenSession` before Noise IK. The responder
//! authenticates the persistent NodeHost static key immediately after message 1
//! and before decoding any encrypted request. The cleartext `GetQuote`
//! preamble exists only in the separate development/mock mode.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_primitives::B256;
use zeroize::Zeroizing;

use outbe_primitives::tee_attestation_v1::{
    AttestationMode, AttestationOperationV1, EnclaveInitializationManifestV1, NetworkBindingV1,
    RegistrationIntentV1, TransitionKeyReadyProofV1,
};
use outbe_tee::codec::{decode_request, encode_response, read_frame, write_frame};
use outbe_tee::errors::TransportError;
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::NOISE_PARAMS;

use crate::dcap_verifier::{
    complete_gramine_direct_dev_onboarding_response, complete_verification_response,
    DcapVerificationProgressV1, DcapVerificationSessionV1,
};
use crate::dkg::{build_ceremony_info, DkgSessionStore};
use crate::initialization::{
    InitializationMode, InitializationState, PendingInitialization, PendingRemoteSessionV1,
    SessionAuthorityV1,
};
use crate::keys::EnclaveKeys;
use crate::onboarding_upload::{
    CompleteOnboardingArtifactIngestV1, OnboardingArtifactUploadProgressV1,
    OnboardingArtifactUploadSessionV1,
};
use crate::process::process_tribute_offer_batch;
use crate::seal::{EnclaveBootConfig, KeyPolicy, SealHeader, SEAL_FORMAT};

mod dispatch;
mod offer_key;
mod server;
mod session;
#[cfg(test)]
mod tests;

pub use dispatch::requests::dispatch;

pub use offer_key::{
    unseal_tribute_offer_and_group_sig_on_boot, DerivedTributeOfferKey, SharedTributeOfferKey,
};

pub use server::{serve, serve_tcp};

pub use session::{serve_connection, serve_connection_with, EnclaveTransportStream};

#[cfg(feature = "mock")]
pub use session::{serve_connection_for_network_test, serve_connection_with_synthetic_dcap};

pub(crate) use offer_key::{sealing_key, write_once_0600};

use dispatch::attestation::validate_generated_quote_binding;

use dispatch::dkg::dispatch_dkg_open;

use dispatch::onboarding::{
    complete_gramine_direct_dev_onboarding_ingest_response,
    complete_onboarding_artifact_ingest_response,
};

use dispatch::requests::{
    dispatch_with_initialization, into_response, DispatchInitializationContext,
};

use offer_key::persist_then_activate_offer_key;

use session::serve_connection_with_resident_chain;

#[cfg(feature = "mock")]
use dispatch::attestation::synthetic_dcap_quote;

#[cfg(test)]
use dispatch::onboarding::derive_onboarding_offer_key_v1;

#[cfg(test)]
use offer_key::persist_offer_key_required;
