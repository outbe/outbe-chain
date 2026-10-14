//! Handle registration requests after session admission.

use super::requests::RequestContext;
use crate::transport::*;

type GeneratedDcapQuoteResponse = (Vec<u8>, Vec<u8>, Vec<u8>);

pub(super) fn dispatch(req: EnclaveRequest, context: RequestContext<'_>) -> EnclaveResponse {
    match req {
        EnclaveRequest::GenerateDcapQuote { intent } => generate_quote(context, intent),
        EnclaveRequest::GenerateTransitionEvidenceDevV1 { intent } => {
            generate_transition_evidence(context, intent)
        }
        EnclaveRequest::SignRegistrationIntentDevV1 { intent } => {
            sign_registration_intent(context, intent)
        }
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}

fn generate_quote(context: RequestContext<'_>, intent: Vec<u8>) -> EnclaveResponse {
    let RequestContext {
        keys,
        offer_key,
        initialization: context,
        ..
    } = context;
    let DispatchInitializationContext {
        initialization,
        quote_generator,
        ..
    } = context;
    let Some(initialization) = initialization else {
        return EnclaveResponse::Error {
            message: "DCAP quote generation requires initialized production state".to_string(),
        };
    };
    let result = (|| -> EnclaveResult<GeneratedDcapQuoteResponse> {
        let report_data = initialization.quote_report_data(&intent)?;
        let decoded_intent = RegistrationIntentV1::decode_canonical(&intent)
            .map_err(TransportDispatchError::registration_intent_not_canonical)?;
        let transition_key_ready_proof = if decoded_intent.operation
            == AttestationOperationV1::TransitionEnclaveMeasurement
        {
            let resident_offer_public = offer_key
                .get()
                .ok_or(TransportDispatchError::TransitionQuoteRequiresOfferKey)?
                .public();
            let manifest = initialization
                .manifest()?
                .ok_or(TransportDispatchError::EnclaveNotInitialized)?;
            let (_, proof) =
                signed_transition_proof(keys, &decoded_intent, &manifest, resident_offer_public)?;
            proof
                .encode_canonical()
                .map_err(TransportDispatchError::detail)?
        } else {
            Vec::new()
        };
        let quote = quote_generator(&report_data)?;
        let quote_body = validate_generated_quote_binding(report_data, quote)?;
        let enclave_signature = keys.sign_attestation(&report_data[..32]);
        Ok((
            quote_body,
            enclave_signature.to_vec(),
            transition_key_ready_proof,
        ))
    })();
    match result {
        Ok((quote_body, enclave_signature, transition_key_ready_proof)) => {
            EnclaveResponse::DcapQuote {
                intent,
                quote_body,
                enclave_signature,
                transition_key_ready_proof,
            }
        }
        Err(message) => EnclaveResponse::Error {
            message: message.to_string(),
        },
    }
}

fn generate_transition_evidence(context: RequestContext<'_>, intent: Vec<u8>) -> EnclaveResponse {
    let RequestContext {
        keys,
        offer_key,
        initialization: context,
        ..
    } = context;
    let DispatchInitializationContext { initialization, .. } = context;
    let result = (|| -> EnclaveResult<Vec<u8>> {
        let initialization =
            initialization.ok_or(TransportDispatchError::TransitionRequiresInitializedEnclave)?;
        if !initialization.gramine_direct_dev_evidence_allowed() {
            return Err(TransportDispatchError::DirectDevTransitionDisabled.into());
        }
        let decoded = RegistrationIntentV1::decode_canonical(&intent)
            .map_err(TransportDispatchError::detail)?;
        if decoded.attestation_mode != AttestationMode::GramineDirectDev
            || decoded.operation != AttestationOperationV1::TransitionEnclaveMeasurement
        {
            return Err(TransportDispatchError::ExpectedDirectDevMeasurementTransition.into());
        }
        let manifest = initialization
            .manifest()?
            .ok_or(TransportDispatchError::EnclaveNotInitialized)?;
        manifest
            .validate_intent_binding(&decoded)
            .map_err(TransportDispatchError::detail)?;
        let resident_offer_public = offer_key
            .get()
            .ok_or(TransportDispatchError::TransitionRequiresPermanentOfferKey)?
            .public();
        let (intent_hash, proof) =
            signed_transition_proof(keys, &decoded, &manifest, resident_offer_public)?;
        outbe_primitives::tee_attestation_v1::AttestationEvidenceV1::GramineDirectDev(
            outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1 {
                intent: decoded,
                dev_attestation_public: keys.attestation_pub(),
                dev_signature: keys.sign_attestation(intent_hash.as_slice()),
                transition_key_ready_proof: Some(proof),
            },
        )
        .encode_canonical()
        .map_err(|e| TransportDispatchError::detail(e).into())
    })();
    match result {
        Ok(evidence) => EnclaveResponse::TransitionEvidenceDevV1 { evidence },
        Err(message) => EnclaveResponse::Error {
            message: message.to_string(),
        },
    }
}

fn sign_registration_intent(context: RequestContext<'_>, intent: Vec<u8>) -> EnclaveResponse {
    let RequestContext {
        keys,
        initialization: context,
        ..
    } = context;
    let DispatchInitializationContext { initialization, .. } = context;
    let Some(initialization) = initialization else {
        return EnclaveResponse::Error {
            message: "development intent signing requires the development transport".to_string(),
        };
    };
    if !initialization.gramine_direct_dev_evidence_allowed() {
        return EnclaveResponse::Error {
            message: "GramineDirectDev evidence requires development mode or production SGX without remote attestation"
                .to_string(),
        };
    }
    let result = (|| -> EnclaveResult<Vec<u8>> {
        let decoded = RegistrationIntentV1::decode_canonical(&intent)
            .map_err(TransportDispatchError::detail)?;
        if decoded.attestation_mode != AttestationMode::GramineDirectDev
            || decoded.attestation_ed25519 != keys.attestation_pub()
            || decoded
                .derived_enclave_id()
                .map_err(TransportDispatchError::detail)?
                != decoded.enclave_id
        {
            return Err(TransportDispatchError::GramineDirectDevIntentIdentityMismatch.into());
        }
        let intent_hash = decoded
            .intent_hash()
            .map_err(TransportDispatchError::detail)?;
        Ok(keys.sign_attestation(intent_hash.as_slice()).to_vec())
    })();
    match result {
        Ok(enclave_signature) => EnclaveResponse::RegistrationIntentSignedDevV1 {
            intent,
            enclave_signature,
        },
        Err(message) => EnclaveResponse::Error {
            message: message.to_string(),
        },
    }
}

fn signed_transition_proof(
    keys: &EnclaveKeys,
    intent: &RegistrationIntentV1,
    manifest: &EnclaveInitializationManifestV1,
    resident_offer_public: [u8; 32],
) -> EnclaveResult<(B256, TransitionKeyReadyProofV1)> {
    let intent_hash = intent
        .intent_hash()
        .map_err(TransportDispatchError::detail)?;
    let mut proof = TransitionKeyReadyProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        transition_intent_hash: intent_hash,
        candidate_manifest_hash: manifest
            .authorization_hash()
            .map_err(TransportDispatchError::detail)?,
        transition_nonce: intent.transition_nonce,
        resident_offer_public,
        candidate_attestation_signature: [0; 64],
    };
    let signing_hash = proof
        .signing_hash()
        .map_err(TransportDispatchError::detail)?;
    proof.candidate_attestation_signature = keys.sign_attestation(signing_hash.as_slice());
    Ok((intent_hash, proof))
}
