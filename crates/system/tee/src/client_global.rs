//! Process-global enclave client shared by every enclave-using module.
//!
//! Production installs an [`AuthorizedEnclaveClient`] after node-signed,
//! write-once initialization; the separate dev/mock path may install the development
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

use alloy_primitives::B256;

use crate::client::{AuthorizedEnclaveClient, EnclaveClient, GeneratedDcapQuoteV1};
use crate::dcap_protocol::{DcapOnboardingVerificationResultV1, DcapVerificationOutcomeV1};
use crate::errors::TransportError;
use crate::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

pub enum RuntimeEnclaveClient {
    Development(Box<EnclaveClient>),
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

/// Install the separate dev/mock client once.
pub fn install_enclave_client(client: EnclaveClient) -> Result<(), &'static str> {
    ENCLAVE_CLIENT
        .set(Mutex::new(RuntimeEnclaveClient::Development(Box::new(
            client,
        ))))
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
///
/// TODO(tee-perf): every enclave call — consensus-path ops (gratis/promis,
/// begin-block sweeps, per-WWD snapshot batches) and read-only queries (e.g.
/// eth_call fidelity index with signed auth) — serializes on this single
/// Mutex-guarded blocking connection. A query storm on an RPC node can stall
/// block execution behind it. Future optimization: split read-only traffic onto
/// a separate enclave connection (or a small pool), and/or rate-limit
/// query-path calls so consensus-path requests never queue behind them.
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

/// Generate one intent-bound quote through the already installed production
/// NodeHost session. The lifecycle worker cannot create a second enclave
/// identity or fall back to the development transport.
pub fn generate_dcap_quote_v1(
    intent: &RegistrationIntentV1,
) -> Result<GeneratedDcapQuoteV1, TransportError> {
    let Some(result) = try_with_enclave(|client| match client {
        RuntimeEnclaveClient::Production(client) => client.generate_dcap_quote(intent),
        RuntimeEnclaveClient::Development(_) => Err(TransportError::Attestation(
            "development enclave client cannot generate production renewal quotes".into(),
        )),
    }) else {
        return Err(TransportError::Attestation(
            "production enclave client is not configured or its lock is poisoned".into(),
        ));
    };
    result
}

/// Invoke the dedicated purpose-bound `RegisterEnclave` verifier and obtain
/// its deterministic one-time onboarding artifact from a production enclave.
#[allow(clippy::too_many_arguments)]
pub fn verify_dcap_registration_and_seal_v1(
    evidence: &[u8],
    policy: &[u8],
    block_timestamp: u64,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
    expected_tribute_offer_public: [u8; 32],
    key_epoch: u64,
    tribute_offer_epoch: u64,
) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
    let Some(result) = try_with_enclave(|client| match client {
        RuntimeEnclaveClient::Production(client) => client.verify_dcap_registration_and_seal_v1(
            evidence,
            policy,
            block_timestamp,
            node_signature,
            enclave_signature,
            expected_tribute_offer_public,
            key_epoch,
            tribute_offer_epoch,
        ),
        RuntimeEnclaveClient::Development(_) => Err(TransportError::DcapVerification(
            "development enclave client cannot issue DCAP onboarding artifacts".into(),
        )),
    }) else {
        return Err(TransportError::DcapVerification(
            "production enclave client is not configured or its lock is poisoned".into(),
        ));
    };
    result
}

/// Read whether the permanent tribute-offer key is ready in the mandatory local
/// enclave. `None` is a fresh/keyless state, not a replacement or recovery path.
pub fn resident_offer_public_key_state_v1() -> Result<Option<B256>, TransportError> {
    let Some(result) = try_with_enclave(|client| client.request(&EnclaveRequest::GetPublicKeys))
    else {
        return Err(TransportError::EnclaveError(
            "mandatory enclave client is not configured or its lock is poisoned".into(),
        ));
    };
    match result? {
        EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            ..
        } => {
            if !offer_key_ready {
                return Ok(None);
            }
            let public = B256::from(recipient_x25519_pub);
            if public.is_zero() {
                return Err(TransportError::EnclaveError(
                    "local enclave reports a ready but zero permanent offer key".into(),
                ));
            }
            Ok(Some(public))
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}

/// Require the permanent tribute-offer key. A keyless enclave is fatal to an
/// existing identity and never triggers recovery, replacement, or fallback.
pub fn resident_offer_public_key_v1() -> Result<B256, TransportError> {
    resident_offer_public_key_state_v1()?.ok_or_else(|| {
        TransportError::EnclaveError(
            "local enclave permanent offer key is not ready; no recovery or fallback exists".into(),
        )
    })
}

/// DETERMINISTICALLY seal the resident tribute offer key to `recipient_x25519` via
/// the enclave (`SealOfferKeyForRegistry`), for committing the sealed blob on-chain
/// (on-chain offer-key delivery to a joining validator). Every committee
/// node's enclave returns the same blob (static-static ECDH), so the on-chain write
/// is consensus-deterministic.
///
/// Returns `Ok(None)` only to represent an unconfigured process. The production V1
/// registry caller maps that state to a fatal execution invariant; it never skips
/// the event. `Ok(Some(blob))` is success and `Err` reports an enclave or transport
/// failure.
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
