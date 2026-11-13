//! Host-side domain enclave protocol, client wrappers, offer encryption, and owner-local opening.
//!
//! This module depends only on `outbe-tee`'s domain-neutral transport, AEAD, and
//! witness infrastructure (`PayloadLane`, `EnclaveRequest::Payload`,
//! `EnclaveResponse::Payload`, `EnclaveWitnessEntryV1::Payload`), without
//! pulling in any in-enclave engines (`outbe-lysis`, `outbe-fidelity-math`,
//! `outbe-zk`, `ark-*`).

pub(crate) mod authenticate;
pub mod balance_client;
pub mod errors;
pub mod fidelity_decrypt;
pub mod gratis_decrypt;
pub mod lysis;
pub mod nod_decrypt;
pub mod nod_mine;
pub mod nod_transport;
pub mod offer_encrypt;
pub mod promis_decrypt;
pub mod protocol;
pub mod response_auth;
pub mod tribute_client;
pub mod tribute_day;
pub mod tribute_day_client;
pub mod tribute_decrypt;
pub mod tribute_source;
pub mod tribute_v2;
pub mod verification;

#[cfg(test)]
mod witness_tests;

use outbe_tee::{
    canary_attestation_pub, canary_request_with_meta, consensus_call, protocol::PayloadLane,
    try_account_query_with_meta, try_with_enclave, AuthorizedEnclaveClient, EnclaveClient,
    EnclaveSession, RuntimeEnclaveClient, TransportError,
};

pub use errors::{BalanceClientError, EnclaveClientError, LocalCryptoError, TributeSourceError};
pub use verification::{
    verify_fidelity_cohort_attestation, verify_fidelity_query_attestation,
    verify_fidelity_snapshot_attestation, verify_gratis_op_attestation,
    verify_mine_gratis_from_promis_attestation, verify_promis_op_attestation,
    verify_tribute_offer_attestation,
};

/// Protocol-fixed 32-byte HKDF-SHA256 salt for Tribute offer and confidential amount ECDHE.
///
/// Value: ASCII `"outbe/tribute/offer-salt/v1"`, zero-padded to 32 bytes.
pub const OFFER_HKDF_SALT: [u8; 32] = *b"outbe/tribute/offer-salt/v1\0\0\0\0\0";

/// Execute a domain [`protocol::EnclaveRequest`] over an [`EnclaveSession`] on `lane`.
pub fn request_enclave_session_on_lane(
    session: &mut EnclaveSession,
    lane: PayloadLane,
    request: &protocol::EnclaveRequest,
) -> Result<protocol::EnclaveResponse, TransportError> {
    let wire_req = request.encode_wire(lane)?;
    let wire_resp = session.request_with_meta(request.call_meta(), &wire_req)?;
    protocol::EnclaveResponse::from_wire_response(&wire_resp)
}

/// Execute a domain [`protocol::EnclaveRequest`] over an [`EnclaveSession`] on [`PayloadLane::Execution`].
pub fn request_enclave_session(
    session: &mut EnclaveSession,
    request: &protocol::EnclaveRequest,
) -> Result<protocol::EnclaveResponse, TransportError> {
    request_enclave_session_on_lane(session, PayloadLane::Execution, request)
}

/// Execute a domain [`protocol::EnclaveRequest`] over a development [`EnclaveClient`] on `lane`.
pub fn request_enclave_client_on_lane(
    client: &mut EnclaveClient,
    lane: PayloadLane,
    request: &protocol::EnclaveRequest,
) -> Result<protocol::EnclaveResponse, TransportError> {
    let wire_req = request.encode_wire(lane)?;
    let wire_resp = client.request(&wire_req)?;
    protocol::EnclaveResponse::from_wire_response(&wire_resp)
}

/// Execute a domain [`protocol::EnclaveRequest`] over a development [`EnclaveClient`] on [`PayloadLane::Execution`].
pub fn request_enclave_client(
    client: &mut EnclaveClient,
    request: &protocol::EnclaveRequest,
) -> Result<protocol::EnclaveResponse, TransportError> {
    request_enclave_client_on_lane(client, PayloadLane::Execution, request)
}

/// Execute a domain [`protocol::EnclaveRequest`] over an [`AuthorizedEnclaveClient`] on `lane`.
pub fn request_authorized_enclave_client_on_lane(
    client: &mut AuthorizedEnclaveClient,
    lane: PayloadLane,
    request: &protocol::EnclaveRequest,
) -> Result<protocol::EnclaveResponse, TransportError> {
    let wire_req = request.encode_wire(lane)?;
    let wire_resp = client.request(&wire_req)?;
    protocol::EnclaveResponse::from_wire_response(&wire_resp)
}

/// Execute a domain [`protocol::EnclaveRequest`] over an [`AuthorizedEnclaveClient`] on [`PayloadLane::Execution`].
pub fn request_authorized_enclave_client(
    client: &mut AuthorizedEnclaveClient,
    request: &protocol::EnclaveRequest,
) -> Result<protocol::EnclaveResponse, TransportError> {
    request_authorized_enclave_client_on_lane(client, PayloadLane::Execution, request)
}

/// Execute a domain [`protocol::EnclaveRequest`] over a [`RuntimeEnclaveClient`] on [`PayloadLane::Execution`].
pub fn request_runtime_enclave_client(
    client: &mut RuntimeEnclaveClient,
    request: &protocol::EnclaveRequest,
) -> Result<protocol::EnclaveResponse, TransportError> {
    let wire_req = request.encode_wire(PayloadLane::Execution)?;
    let wire_resp = client.request_with_meta(request.call_meta(), &wire_req)?;
    protocol::EnclaveResponse::from_wire_response(&wire_resp)
}

/// Run a non-consensus domain request against the process-global execution session.
///
/// Returns `None` only when no enclave session is installed.
pub fn try_with_domain_enclave(
    request: &protocol::EnclaveRequest,
) -> Option<([u8; 32], Result<protocol::EnclaveResponse, TransportError>)> {
    try_with_enclave(|session| {
        (
            session.attestation_pub(),
            request_enclave_session(session, request),
        )
    })
}

/// Run an authorized account-key RPC query (`DeriveAccountKeys`) on the dedicated account-query lane.
///
/// Returns `None` only when no enclave session is installed.
pub fn try_domain_account_query(
    request: &protocol::EnclaveRequest,
) -> Option<Result<protocol::EnclaveResponse, TransportError>> {
    let wire_req = match request.encode_wire(PayloadLane::AccountQuery) {
        Ok(wire_req) => wire_req,
        Err(err) => return Some(Err(err)),
    };
    let wire_result = try_account_query_with_meta(request.call_meta(), &wire_req)?;
    Some(
        wire_result.and_then(|wire_resp| protocol::EnclaveResponse::from_wire_response(&wire_resp)),
    )
}

/// Run a read-only canary domain probe (`ProcessTributeOfferBatch` / `ProcessEncryptedTributeOfferBatchV2`)
/// on the dedicated canary lane, returning `(pinned_attestation_pub, response)`.
pub fn canary_domain_request(
    request: &protocol::EnclaveRequest,
) -> Result<([u8; 32], protocol::EnclaveResponse), TransportError> {
    let key = canary_attestation_pub()
        .ok_or(outbe_tee::errors::TeeClientVerificationError::EnclaveSessionNotConfigured)?;
    let wire_req = request.encode_wire(PayloadLane::Canary)?;
    let wire_resp = canary_request_with_meta(request.call_meta(), &wire_req)?;
    let decoded = protocol::EnclaveResponse::from_wire_response(&wire_resp)?;
    Ok((key, decoded))
}

/// Execute a consensus-witness-aware domain enclave operation.
///
/// - On `ProposerRecord`, encodes `request` into [`PayloadLane::Execution`], strips
///   every inner `attestation_tag` inside `Payload.payload`, authenticates against
///   `leader_key`, and appends `EnclaveWitnessEntryV1::Payload` to the witness tape.
/// - On `VerifierReplay`, reconstructs `WireResponse::Payload` from the witness tape
///   without touching local transport, canonically decodes `protocol::EnclaveResponse`,
///   and verifies `authenticate(&leader_key, &decoded)`.
/// - On `VerifierWithEnclave`, verifies both the proposer's witness entry and the
///   local enclave's response, enforcing byte-identical tag-free `Payload.payload`
///   and `inputs_canonical_hash` even when `verifier_key != leader_key`.
pub fn consensus_domain_call(
    label: &'static str,
    request: &protocol::EnclaveRequest,
    missing_sidecar_err: impl FnOnce() -> TransportError,
    authenticate: impl Fn(&[u8; 32], &protocol::EnclaveResponse) -> Result<(), TransportError>,
) -> Result<([u8; 32], protocol::EnclaveResponse), TransportError> {
    let (key, wire_resp) = consensus_call(
        label,
        || {
            #[cfg(any(test, feature = "test-utils"))]
            if let Some(hook_result) = tribute_client::try_test_local_enclave_hook(request) {
                let (hook_key, domain_resp) = hook_result?;
                return Ok((hook_key, domain_resp.into_wire_response()?));
            }
            let wire_req = request.encode_wire(PayloadLane::Execution)?;
            let meta = request.call_meta();
            let (session_key, wire_result) = try_with_enclave(|session| {
                (
                    session.attestation_pub(),
                    session.request_with_meta(meta, &wire_req),
                )
            })
            .ok_or_else(missing_sidecar_err)?;
            Ok((session_key, wire_result?))
        },
        |verify_key, wire_resp| {
            let decoded = protocol::EnclaveResponse::from_wire_response(wire_resp)?;
            authenticate(verify_key, &decoded)
        },
    )?;
    let decoded = protocol::EnclaveResponse::from_wire_response(&wire_resp)?;
    Ok((key, decoded))
}
