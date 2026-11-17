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
pub mod nod_decrypt;
pub mod nod_mine;
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
mod v2_balance_decrypt;
pub mod verification;

#[cfg(test)]
mod witness_tests;

use outbe_tee::{
    in_process_tee_context,
    protocol::{EnclaveResponse as WireResponse, PayloadLane},
    tee_consensus_call, tee_session_call, try_account_query_with_meta, try_in_process_request,
    CallMeta, InProcessDispatchError, TransportError,
};

pub use errors::{
    BalanceClientError, EnclaveClientError, LocalCryptoError, TributeSourceError,
    FIDELITY_ATTESTATION, GRATIS_OP_ATTESTATION, NOD_MINT_REJECTED, PROMIS_OP_ATTESTATION,
    TRIBUTE_OFFER_ATTESTATION, TRIBUTE_SOURCE_REJECTED,
};
pub use offer_encrypt::OFFER_HKDF_SALT;
pub use promis_decrypt::decrypt_promis_balance;
#[cfg(any(test, feature = "test-utils"))]
pub use tribute_client::set_test_local_enclave_hook;
pub use verification::{
    verify_fidelity_cohort_attestation, verify_fidelity_query_attestation,
    verify_fidelity_snapshot_attestation, verify_gratis_op_attestation,
    verify_mine_gratis_from_promis_attestation, verify_promis_op_attestation,
    verify_tribute_offer_attestation,
};

/// Run a non-consensus domain request against the resident execution lane.
///
/// Returns `None` only when no resident enclave is installed.
pub fn try_with_domain_enclave(
    request: &protocol::EnclaveRequest,
) -> Option<([u8; 32], Result<protocol::EnclaveResponse, TransportError>)> {
    let context = in_process_tee_context()?;
    let response = request
        .encode_wire(PayloadLane::Execution)
        .and_then(|wire| {
            try_in_process_request(request.call_meta(), &wire)
                .ok_or(InProcessDispatchError::ContextNotInstalled)?
                .and_then(|response| protocol::EnclaveResponse::from_wire_response(&response))
        });
    Some((context.attestation_public(), response))
}

/// Run an authorized account-key RPC query (`DeriveAccountKeys`) on the dedicated account-query lane.
///
/// Returns `None` only when no resident enclave is installed.
pub fn try_domain_account_query(
    request: &protocol::EnclaveRequest,
) -> Option<Result<protocol::EnclaveResponse, TransportError>> {
    if in_process_tee_context().is_some() && !request.allows_lane(PayloadLane::AccountQuery) {
        return Some(Err(InProcessDispatchError::LaneDenied {
            lane: PayloadLane::AccountQuery,
        }
        .into()));
    }
    let wire_req = match request.encode_wire(PayloadLane::AccountQuery) {
        Ok(wire_req) => wire_req,
        Err(err) => return Some(Err(err)),
    };
    let wire_result = try_account_query_with_meta(request.call_meta(), &wire_req)?;
    Some(
        wire_result.and_then(|wire_resp| protocol::EnclaveResponse::from_wire_response(&wire_resp)),
    )
}

/// Exercise the resident canary lane for domain lane-isolation tests.
#[cfg(test)]
fn canary_domain_request(
    request: &protocol::EnclaveRequest,
) -> Result<([u8; 32], protocol::EnclaveResponse), TransportError> {
    let key =
        outbe_tee::canary_attestation_pub().ok_or(InProcessDispatchError::ContextNotInstalled)?;
    if !request.allows_lane(PayloadLane::Canary) {
        return Err(InProcessDispatchError::LaneDenied {
            lane: PayloadLane::Canary,
        }
        .into());
    }
    let wire_req = request.encode_wire(PayloadLane::Canary)?;
    let wire_resp = outbe_tee::canary_request_with_meta(request.call_meta(), &wire_req)?;
    let decoded = protocol::EnclaveResponse::from_wire_response(&wire_resp)?;
    Ok((key, decoded))
}

/// Execute an authenticated non-consensus domain operation on the resident execution lane.
///
/// Returns `None` only when no resident enclave is installed.
pub fn domain_session_call(
    request: &protocol::EnclaveRequest,
    authenticate: impl FnOnce(&[u8; 32], &protocol::EnclaveResponse) -> Result<(), TransportError>,
) -> Option<Result<([u8; 32], protocol::EnclaveResponse), TransportError>> {
    let meta = request.call_meta();
    if meta.recordable {
        return Some(Err(TransportError::Codec(
            "recordable domain request routed to non-consensus domain_session_call".into(),
        )));
    }
    let wire_req = match request.encode_wire(PayloadLane::Execution) {
        Ok(wire_req) => wire_req,
        Err(err) => return Some(Err(err)),
    };
    let mut decoded_out = None;
    let wire_result = tee_session_call(meta, &wire_req, |verify_key, wire_resp| {
        let decoded = protocol::EnclaveResponse::from_wire_response(wire_resp)?;
        authenticate(verify_key, &decoded)?;
        decoded_out = Some(decoded);
        Ok(())
    })?;
    Some(wire_result.and_then(|(key, _)| {
        let decoded = decoded_out.ok_or(TransportError::UnexpectedResponse)?;
        Ok((key, decoded))
    }))
}

fn verify_consensus_domain_response(
    verify_key: &[u8; 32],
    wire_resp: &WireResponse,
    authenticate: &impl Fn(&[u8; 32], &protocol::EnclaveResponse) -> Result<(), TransportError>,
) -> Result<(), TransportError> {
    let WireResponse::Payload { recordable, .. } = wire_resp else {
        return Err(TransportError::UnexpectedResponse);
    };
    let decoded = protocol::EnclaveResponse::from_wire_response(wire_resp)?;
    if !(decoded.is_recordable() && *recordable) {
        return Err(TransportError::Codec(
            "non-recordable enclave response on consensus path".into(),
        ));
    }
    authenticate(verify_key, &decoded)
}

/// Execute a consensus-witness-aware domain enclave operation.
///
/// - On `ProposerRecord`, encodes `request` into [`PayloadLane::Execution`], strips
///   every inner `attestation_tag` inside `Payload.payload`, authenticates against
///   `leader_key`, and appends `EnclaveWitnessEntryV1::Payload` to the witness tape.
/// - On `VerifierReplay`, reconstructs `WireResponse::Payload` from the witness tape
///   without calling the resident enclave, canonically decodes `protocol::EnclaveResponse`,
///   and verifies `authenticate(&leader_key, &decoded)`.
/// - On `VerifierWithEnclave`, verifies both the proposer's witness entry and the
///   local enclave's response, enforcing byte-identical tag-free `Payload.payload`
///   and `inputs_canonical_hash` even when `verifier_key != leader_key`.
pub fn consensus_domain_call(
    label: &'static str,
    request: &protocol::EnclaveRequest,
    missing_enclave_err: impl FnOnce() -> TransportError,
    authenticate: impl Fn(&[u8; 32], &protocol::EnclaveResponse) -> Result<(), TransportError>,
) -> Result<([u8; 32], protocol::EnclaveResponse), TransportError> {
    let meta = CallMeta {
        label,
        ..request.call_meta()
    };
    if !meta.recordable {
        return Err(TransportError::Codec(
            "non-recordable domain request routed to consensus_domain_call".into(),
        ));
    }
    let wire_req = request.encode_wire(PayloadLane::Execution)?;
    #[cfg(any(test, feature = "test-utils"))]
    let (key, wire_resp) = if tribute_client::has_test_local_enclave_hook() {
        outbe_tee::consensus_call(
            meta.label,
            || {
                let hook_result = tribute_client::try_test_local_enclave_hook(request)
                    .ok_or_else(missing_enclave_err)?;
                let (hook_key, domain_resp) = hook_result?;
                Ok((hook_key, domain_resp.into_wire_response()?))
            },
            |verify_key, wire_resp| {
                verify_consensus_domain_response(verify_key, wire_resp, &authenticate)
            },
        )?
    } else {
        tee_consensus_call(meta, &wire_req, missing_enclave_err, |verify_key, resp| {
            verify_consensus_domain_response(verify_key, resp, &authenticate)
        })?
    };
    #[cfg(not(any(test, feature = "test-utils")))]
    let (key, wire_resp) =
        tee_consensus_call(meta, &wire_req, missing_enclave_err, |verify_key, resp| {
            verify_consensus_domain_response(verify_key, resp, &authenticate)
        })?;
    let decoded = protocol::EnclaveResponse::from_wire_response(&wire_resp)?;
    Ok((key, decoded))
}

#[cfg(feature = "engine")]
pub mod domain_in_process;
