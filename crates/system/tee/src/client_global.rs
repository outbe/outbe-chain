//! Process-global enclave client shared by every enclave-using module.
//!
//! Production installs an [`AuthorizedEnclaveClient`] after node-signed,
//! write-once initialization; the separate dev/mock path may install the legacy
//! [`EnclaveClient`]. Both expose only requests and the manifest/quote-bound
//! attestation key needed by runtime consumers. The offer-decrypt and key-delivery
//! paths reach the single connection through [`try_with_enclave`]. TEE transport
//! infrastructure lives here rather than in a business module.
//!
//! Determinism: the enclave returns byte-identical output across validators (same
//! resident keys), so routing a request through this global does not affect
//! consensus determinism. The call is a blocking UDS/TCP round-trip made straight
//! from the execution path; it never holds a `StorageHandle` across it and never
//! spawns a thread.

use std::sync::{Mutex, OnceLock};

use crate::client::{AuthorizedEnclaveClient, EnclaveClient};
use crate::dcap_protocol::DcapVerificationOutcomeV1;
use crate::errors::TransportError;
use crate::protocol::{EnclaveRequest, EnclaveResponse};

pub enum RuntimeEnclaveClient {
    Development(EnclaveClient),
    Production(AuthorizedEnclaveClient),
}

impl RuntimeEnclaveClient {
    pub fn request(&mut self, request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        match self {
            Self::Development(client) => client.request(request),
            Self::Production(client) => client.request(request),
        }
    }

    pub fn attestation_pub(&self) -> [u8; 32] {
        match self {
            Self::Development(client) => client.attestation_pub(),
            Self::Production(client) => client.attestation_pub(),
        }
    }
}

static ENCLAVE_CLIENT: OnceLock<Mutex<RuntimeEnclaveClient>> = OnceLock::new();

/// True once a process-global enclave client is installed.
pub fn is_enclave_configured() -> bool {
    ENCLAVE_CLIENT.get().is_some()
}

/// Install the separate dev/mock legacy client once.
pub fn install_enclave_client(client: EnclaveClient) -> Result<(), &'static str> {
    ENCLAVE_CLIENT
        .set(Mutex::new(RuntimeEnclaveClient::Development(client)))
        .map_err(|_| "enclave client already initialized")
}

/// Install a production NodeHost-authorized client once. Initialization and
/// manifest validation are completed by `AuthorizedEnclaveClient` before this.
pub fn install_authorized_enclave_client(
    client: AuthorizedEnclaveClient,
) -> Result<(), &'static str> {
    ENCLAVE_CLIENT
        .set(Mutex::new(RuntimeEnclaveClient::Production(client)))
        .map_err(|_| "enclave client already initialized")
}
/// Run `f` against the process-global enclave client. Returns `None` if no client
/// is configured or the mutex is poisoned (the caller maps that to a typed
/// `tee_sidecar_unavailable` error).
pub fn try_with_enclave<R>(f: impl FnOnce(&mut RuntimeEnclaveClient) -> R) -> Option<R> {
    let mutex = ENCLAVE_CLIENT.get()?;
    let mut client = mutex.lock().ok()?;
    Some(f(&mut client))
}

/// Invoke the full verifier only through a production NodeHost-authorized
/// Gramine enclave. Missing/poisoned/development clients are local fatal inputs
/// to the consensus caller, never deterministic evidence rejection.
pub fn verify_dcap_evidence_v1(
    evidence: &[u8],
    policy: &[u8],
    block_timestamp: u64,
) -> Result<DcapVerificationOutcomeV1, TransportError> {
    let Some(result) = try_with_enclave(|client| match client {
        RuntimeEnclaveClient::Production(client) => {
            client.verify_dcap_evidence_v1(evidence, policy, block_timestamp)
        }
        RuntimeEnclaveClient::Development(_) => Err(TransportError::DcapVerification(
            "development enclave client cannot verify consensus DCAP evidence".into(),
        )),
    }) else {
        return Err(TransportError::DcapVerification(
            "production enclave client is not configured or its lock is poisoned".into(),
        ));
    };
    result
}

/// DETERMINISTICALLY seal the resident tribute offer key to `recipient_x25519` via
/// the enclave (`SealOfferKeyForRegistry`), for committing the sealed blob on-chain
/// (on-chain offer-key delivery to a joining validator). Every committee
/// node's enclave returns the same blob (static-static ECDH), so the on-chain write
/// is consensus-deterministic.
///
/// Returns `Ok(None)` when no enclave is configured (non-TEE node — the caller skips
/// the seal), `Ok(Some(blob))` on success, and `Err` when the enclave is configured
/// but the seal failed (e.g. no resident offer key yet, or the sidecar errored).
pub fn seal_offer_key_for_registry(recipient_x25519: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
    let Some(result) = try_with_enclave(|client| {
        client.request(&EnclaveRequest::SealOfferKeyForRegistry { recipient_x25519 })
    }) else {
        return Ok(None);
    };
    match result.map_err(|e| format!("enclave SealOfferKeyForRegistry transport error: {e}"))? {
        EnclaveResponse::SealedOfferKeyForRegistry { sealed } => Ok(Some(sealed)),
        EnclaveResponse::Error { message } => Err(format!(
            "enclave refused SealOfferKeyForRegistry: {message}"
        )),
        other => Err(format!(
            "unexpected enclave response to SealOfferKeyForRegistry: {other:?}"
        )),
    }
}
