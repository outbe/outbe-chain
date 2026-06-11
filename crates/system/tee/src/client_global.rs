//! Process-global enclave client shared by every enclave-using module.
//!
//! The host connects to the enclave sidecar once at startup (attesting its quote)
//! and installs the verified [`EnclaveClient`] here. Both the offer-decrypt path
//! (`tributefactory`) and the on-chain offer-key delivery (`teeregistry`) then
//! reach the single connection through [`try_with_enclave`] — the enclave client is
//! TEE infrastructure, so it lives in `outbe-tee`, not in any business module.
//!
//! Determinism: the enclave returns byte-identical output across validators (same
//! resident keys), so routing a request through this global does not affect
//! consensus determinism. The call is a blocking UDS/TCP round-trip made straight
//! from the execution path; it never holds a `StorageHandle` across it and never
//! spawns a thread.

use std::sync::{Mutex, OnceLock};

use crate::client::EnclaveClient;
use crate::protocol::{EnclaveRequest, EnclaveResponse};

static ENCLAVE_CLIENT: OnceLock<Mutex<EnclaveClient>> = OnceLock::new();

/// True once a process-global enclave client is installed.
pub fn is_enclave_configured() -> bool {
    ENCLAVE_CLIENT.get().is_some()
}

/// Install the verified process-global enclave client (once). The connect +
/// attestation verification is the caller's responsibility; this only stores the
/// client so every enclave-using module shares one connection.
pub fn install_enclave_client(client: EnclaveClient) -> Result<(), &'static str> {
    ENCLAVE_CLIENT
        .set(Mutex::new(client))
        .map_err(|_| "enclave client already initialized")
}

/// Run `f` against the process-global enclave client. Returns `None` if no client
/// is configured or the mutex is poisoned (the caller maps that to a typed
/// `tee_sidecar_unavailable` error).
pub fn try_with_enclave<R>(f: impl FnOnce(&mut EnclaveClient) -> R) -> Option<R> {
    let mutex = ENCLAVE_CLIENT.get()?;
    let mut client = mutex.lock().ok()?;
    Some(f(&mut client))
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
