use super::*;

pub(super) fn validate_dcap_verification_response(
    attestation_pub: [u8; 32],
    expected_request_hash: B256,
    response: EnclaveResponse,
) -> Result<DcapVerificationOutcomeV1, TransportError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash,
        outcome,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if request_hash != expected_request_hash {
        return Err(TransportError::DcapVerification(
            "enclave response request commitment mismatch".into(),
        ));
    }
    let canonical = DcapVerificationOutcomeV1::decode_canonical(&outcome).map_err(|code| {
        TransportError::DcapVerification(format!(
            "enclave response outcome is non-canonical: {:#06x}",
            code.code()
        ))
    })?;
    let preimage =
        dcap_verification_attestation_preimage(request_hash, &outcome).map_err(|code| {
            TransportError::DcapVerification(format!(
                "enclave response outcome is oversized: {:#06x}",
                code.code()
            ))
        })?;
    let verifying_key = VerifyingKey::from_bytes(&attestation_pub).map_err(|error| {
        TransportError::DcapVerification(format!("invalid enclave attestation key: {error}"))
    })?;
    let signature_bytes: [u8; 64] = attestation_tag.try_into().map_err(|tag: Vec<u8>| {
        TransportError::DcapVerification(format!(
            "invalid enclave verification tag length {}",
            tag.len()
        ))
    })?;
    verifying_key
        .verify_strict(&preimage, &Signature::from_bytes(&signature_bytes))
        .map_err(|error| {
            TransportError::DcapVerification(format!(
                "enclave verification tag is invalid: {error}"
            ))
        })?;
    Ok(canonical)
}

pub(super) fn validate_dcap_onboarding_response(
    attestation_pub: [u8; 32],
    expected_request_hash: B256,
    response: EnclaveResponse,
) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
        request_hash,
        outcome,
        onboarding_artifact,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if request_hash != expected_request_hash {
        return Err(TransportError::DcapVerification(
            "enclave onboarding response request commitment mismatch".into(),
        ));
    }
    let verified = decode_onboarding_result(&outcome, &onboarding_artifact)?;
    let preimage =
        dcap_onboarding_attestation_preimage(request_hash, &outcome, &onboarding_artifact)
            .map_err(|code| {
                TransportError::DcapVerification(format!(
                    "enclave onboarding response is oversized: {:#06x}",
                    code.code()
                ))
            })?;
    let verifying_key = VerifyingKey::from_bytes(&attestation_pub).map_err(|error| {
        TransportError::DcapVerification(format!("invalid enclave attestation key: {error}"))
    })?;
    let signature_bytes: [u8; 64] = attestation_tag.try_into().map_err(|tag: Vec<u8>| {
        TransportError::DcapVerification(format!(
            "invalid enclave onboarding tag length {}",
            tag.len()
        ))
    })?;
    verifying_key
        .verify_strict(&preimage, &Signature::from_bytes(&signature_bytes))
        .map_err(|error| {
            TransportError::DcapVerification(format!("enclave onboarding tag is invalid: {error}"))
        })?;
    Ok(verified)
}

fn decode_onboarding_result(
    outcome: &[u8],
    onboarding_artifact: &[u8],
) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
    let canonical = DcapVerificationOutcomeV1::decode_canonical(outcome).map_err(|code| {
        TransportError::DcapVerification(format!(
            "enclave onboarding outcome is non-canonical: {:#06x}",
            code.code()
        ))
    })?;
    let artifact = if onboarding_artifact.is_empty() {
        None
    } else {
        Some(
            DcapOnboardingArtifactV1::decode_canonical(onboarding_artifact).map_err(|code| {
                TransportError::DcapVerification(format!(
                    "enclave onboarding artifact is non-canonical: {:#06x}",
                    code.code()
                ))
            })?,
        )
    };
    match (&canonical, &artifact) {
        (DcapVerificationOutcomeV1::Accepted(_), Some(_))
        | (DcapVerificationOutcomeV1::Rejected(_), None) => {}
        _ => {
            return Err(TransportError::DcapVerification(
                "enclave onboarding outcome/artifact combination is invalid".into(),
            ))
        }
    }
    Ok(DcapOnboardingVerificationResultV1 {
        outcome: canonical,
        artifact,
    })
}

pub(super) fn validate_generated_dcap_quote(
    intent: &RegistrationIntentV1,
    canonical_intent: &[u8],
    expected_attestation_pub: [u8; 32],
    response: EnclaveResponse,
) -> Result<GeneratedDcapQuoteV1, TransportError> {
    if intent.attestation_ed25519 != expected_attestation_pub {
        return Err(TransportError::Attestation(
            "DCAP quote response key does not match the initialized enclave".into(),
        ));
    }
    let EnclaveResponse::DcapQuote {
        intent: echoed_intent,
        quote_body,
        enclave_signature,
        transition_key_ready_proof,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if echoed_intent != canonical_intent {
        return Err(TransportError::Attestation(
            "DCAP quote response echoed a different registration intent".into(),
        ));
    }
    if quote_body.is_empty() {
        return Err(TransportError::Attestation(
            "DCAP quote response is empty".into(),
        ));
    }
    let enclave_signature: [u8; 64] = enclave_signature.try_into().map_err(|_| {
        TransportError::Attestation(
            "DCAP quote enclave proof-of-possession signature must be 64 bytes".into(),
        )
    })?;
    if !intent.verify_enclave_signature(&enclave_signature) {
        return Err(TransportError::Attestation(
            "DCAP quote enclave proof of possession is invalid".into(),
        ));
    }
    let transition_key_ready_proof =
        validate_transition_key_ready_proof(intent, &transition_key_ready_proof)?;
    Ok(GeneratedDcapQuoteV1 {
        quote_body,
        enclave_signature,
        transition_key_ready_proof,
    })
}

fn validate_transition_key_ready_proof(
    intent: &RegistrationIntentV1,
    encoded: &[u8],
) -> Result<Option<TransitionKeyReadyProofV1>, TransportError> {
    let transition_key_ready_proof = if encoded.is_empty() {
        None
    } else {
        Some(
            TransitionKeyReadyProofV1::decode_canonical(encoded)
                .map_err(|error| TransportError::Codec(error.to_string()))?,
        )
    };
    match (intent.operation, transition_key_ready_proof.as_ref()) {
        (AttestationOperationV1::TransitionEnclaveMeasurement, Some(proof)) => proof
            .verify_for_transition(intent, proof.resident_offer_public)
            .map_err(|error| TransportError::Attestation(error.to_string()))?,
        (AttestationOperationV1::TransitionEnclaveMeasurement, None) => {
            return Err(TransportError::Attestation(
                "transition quote response is missing its key-ready proof".into(),
            ));
        }
        (_, Some(_)) => {
            return Err(TransportError::Attestation(
                "non-transition quote response carries a key-ready proof".into(),
            ));
        }
        (_, None) => {}
    }
    Ok(transition_key_ready_proof)
}

/// Public keys carried in a quote response and bound together through REPORT_DATA.
/// This structural validation does not perform DCAP signature verification or
/// establish production enclave admission. Returned by [`verify_peer_quote`].
#[derive(Clone, Copy, Debug)]
pub struct AttestedPeerKeys {
    pub recipient_x25519: [u8; 32],
    pub attestation_pub: [u8; 32],
    pub noise_static_pub: [u8; 32],
}

/// Validate an enclave quote response and return its bound keys. The connect
/// path pins `noise_static_pub`. Callers may also bind a one-time registration
/// recipient without gaining any peer key-recovery surface.
///
/// Chain:
/// 1. The cleartext public keys must hash to `report_data` (key binding).
/// 2. For a non-empty quote, cleartext measurements and report_data must match
///    the fields parsed from the quote.
///
/// Empty quotes are accepted for development transports. The enclave-resident
/// native QVL and TeeRegistry enforce production attestation separately.
pub fn verify_peer_quote(quote: &EnclaveResponse) -> Result<AttestedPeerKeys, TransportError> {
    let EnclaveResponse::Quote {
        mrenclave,
        mrsigner,
        isv_svn,
        report_data,
        recipient_x25519_pub,
        attestation_pub,
        noise_static_pub,
        quote_body,
        attestation: _,
    } = quote
    else {
        return Err(TransportError::UnexpectedResponse);
    };

    // (1) REPORT_DATA binds the cleartext public keys to the attestation.
    let mut preimage = Vec::with_capacity(96);
    preimage.extend_from_slice(noise_static_pub);
    preimage.extend_from_slice(recipient_x25519_pub);
    preimage.extend_from_slice(attestation_pub);
    let binding = keccak256(&preimage);
    if binding != *report_data {
        return Err(TransportError::Attestation(
            "report_data key binding mismatch".to_string(),
        ));
    }

    // (2) A non-empty quote must be structurally consistent with the cleartext
    // fields. Signature and TCB verification belongs to the native QVL path.
    if !quote_body.is_empty() {
        let m = crate::quote::parse_quote_measurements(quote_body)
            .map_err(|e| TransportError::Attestation(format!("quote parse: {e}")))?;
        if B256::from(m.mrenclave) != *mrenclave
            || B256::from(m.mrsigner) != *mrsigner
            || m.isv_svn != *isv_svn
        {
            return Err(TransportError::Attestation(
                "cleartext measurements do not match the quote".to_string(),
            ));
        }
        if m.report_data[..32] != binding.as_slice()[..] {
            return Err(TransportError::Attestation(
                "quote report_data does not match the key binding".to_string(),
            ));
        }
    }

    Ok(AttestedPeerKeys {
        recipient_x25519: *recipient_x25519_pub,
        attestation_pub: *attestation_pub,
        noise_static_pub: *noise_static_pub,
    })
}

/// Validate the quote bindings and return the enclave Noise static public key to
/// pin in the connect path. Thin wrapper over [`verify_peer_quote`].
pub(super) fn verify_quote(quote: &EnclaveResponse) -> Result<[u8; 32], TransportError> {
    Ok(verify_peer_quote(quote)?.noise_static_pub)
}

/// Shared Ed25519 check over an already domain-separated attestation preimage.
/// The caller selects the public error variant so the API keeps its exact
/// domain-specific error text and classification.
fn verify_attestation_tag<P: AsRef<[u8]>>(
    attestation_pub: &[u8; 32],
    preimage: impl FnOnce() -> P,
    tag: &[u8],
    error: fn(String) -> TransportError,
) -> Result<(), TransportError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let vk = VerifyingKey::from_bytes(attestation_pub)
        .map_err(|e| error(format!("bad attestation key: {e}")))?;
    let sig_bytes: [u8; 64] = tag
        .try_into()
        .map_err(|_| error(format!("bad tag length {}", tag.len())))?;
    let sig = Signature::from_bytes(&sig_bytes);
    vk.verify_strict(preimage().as_ref(), &sig)
        .map_err(|e| error(format!("signature invalid: {e}")))
}

/// Verify a per-offer attestation tag against the peer's session attestation
/// public key. The tag is an Ed25519 signature over
/// [`crate::protocol::tribute_offer_attestation_preimage`]. [`verify_peer_quote`]
/// binds the key to the quote report data. This proves that the session-key
/// holder signed the results. It is not an independent enclave-attestation
/// verdict. The tag is never persisted. Returns a typed error on any mismatch.
pub fn verify_tribute_offer_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    results: &[crate::protocol::TributeOfferResult],
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_attestation_tag(
        attestation_pub,
        || crate::protocol::tribute_offer_attestation_preimage(inputs_canonical_hash, results),
        tag,
        TransportError::TributeOfferAttestation,
    )
}

/// Verify a Gratis-op attestation tag against the peer's session attestation
/// key. The tag is an Ed25519 signature over
/// [`crate::protocol::gratis_op_attestation_preimage`]. Same session-key-holder
/// and verify-then-discard semantics as [`verify_tribute_offer_attestation`].
/// The tag is never persisted.
pub fn verify_gratis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &crate::protocol::GratisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_attestation_tag(
        attestation_pub,
        || crate::protocol::gratis_op_attestation_preimage(inputs_canonical_hash, result),
        tag,
        TransportError::GratisOpAttestation,
    )
}

/// Verify a Promis-op attestation tag. This is the
/// [`verify_gratis_op_attestation`] analogue over
/// [`crate::protocol::promis_op_attestation_preimage`]. Same session-key-holder
/// and verify-then-discard semantics. The tag is never persisted.
pub fn verify_promis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &crate::protocol::PromisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_attestation_tag(
        attestation_pub,
        || crate::protocol::promis_op_attestation_preimage(inputs_canonical_hash, result),
        tag,
        TransportError::PromisOpAttestation,
    )
}

fn verify_fidelity_tag(
    attestation_pub: &[u8; 32],
    preimage: &[u8],
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_attestation_tag(
        attestation_pub,
        || preimage,
        tag,
        TransportError::FidelityAttestation,
    )
}

/// Verify a standalone Fidelity cohort-op attestation tag. Same
/// verify-then-discard semantics as [`verify_gratis_op_attestation`].
pub fn verify_fidelity_cohort_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &crate::protocol::FidelityCohortResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    let preimage =
        crate::protocol::fidelity_cohort_attestation_preimage(inputs_canonical_hash, result);
    verify_fidelity_tag(attestation_pub, &preimage, tag)
}

/// Verify a Fidelity league-snapshot attestation tag.
pub fn verify_fidelity_snapshot_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    leagues: &[crate::protocol::FidelityLeagueEntry],
    tag: &[u8],
) -> Result<(), TransportError> {
    let preimage =
        crate::protocol::fidelity_snapshot_attestation_preimage(inputs_canonical_hash, leagues);
    verify_fidelity_tag(attestation_pub, &preimage, tag)
}

/// Verify a Fidelity index-query attestation tag.
pub fn verify_fidelity_query_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &crate::protocol::FidelityQueryResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    let preimage =
        crate::protocol::fidelity_query_attestation_preimage(inputs_canonical_hash, result);
    verify_fidelity_tag(attestation_pub, &preimage, tag)
}
