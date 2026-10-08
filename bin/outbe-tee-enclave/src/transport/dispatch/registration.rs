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
    let result = (|| -> Result<GeneratedDcapQuoteResponse, String> {
        let report_data = initialization.quote_report_data(&intent)?;
        let decoded_intent = RegistrationIntentV1::decode_canonical(&intent)
            .map_err(|error| format!("registration intent is not canonical: {error}"))?;
        let transition_key_ready_proof = if decoded_intent.operation
            == AttestationOperationV1::TransitionEnclaveMeasurement
        {
            let resident_offer_public = offer_key
                .get()
                .ok_or_else(|| {
                    "transition quote generation requires the permanent offer key".to_string()
                })?
                .public();
            let manifest = initialization
                .manifest()?
                .ok_or_else(|| "enclave is not initialized".to_string())?;
            let (_, proof) =
                signed_transition_proof(keys, &decoded_intent, &manifest, resident_offer_public)?;
            proof
                .encode_canonical()
                .map_err(|error| error.to_string())?
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
        Err(message) => EnclaveResponse::Error { message },
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
    let result = (|| -> Result<Vec<u8>, String> {
        let initialization = initialization.ok_or("transition requires initialized enclave")?;
        if !initialization.gramine_direct_dev_evidence_allowed() {
            return Err("DirectDev transition is disabled for this enclave mode".into());
        }
        let decoded = RegistrationIntentV1::decode_canonical(&intent).map_err(|e| e.to_string())?;
        if decoded.attestation_mode != AttestationMode::GramineDirectDev
            || decoded.operation != AttestationOperationV1::TransitionEnclaveMeasurement
        {
            return Err("expected a DirectDev measurement transition".into());
        }
        let manifest = initialization
            .manifest()?
            .ok_or("enclave is not initialized")?;
        manifest
            .validate_intent_binding(&decoded)
            .map_err(|e| e.to_string())?;
        let resident_offer_public = offer_key
            .get()
            .ok_or("transition requires the permanent offer key")?
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
        .map_err(|e| e.to_string())
    })();
    match result {
        Ok(evidence) => EnclaveResponse::TransitionEvidenceDevV1 { evidence },
        Err(message) => EnclaveResponse::Error { message },
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
    let result = (|| -> Result<Vec<u8>, String> {
        let decoded =
            RegistrationIntentV1::decode_canonical(&intent).map_err(|error| error.to_string())?;
        if decoded.attestation_mode != AttestationMode::GramineDirectDev
            || decoded.attestation_ed25519 != keys.attestation_pub()
            || decoded
                .derived_enclave_id()
                .map_err(|error| error.to_string())?
                != decoded.enclave_id
        {
            return Err("GramineDirectDev intent does not bind this enclave identity".into());
        }
        let intent_hash = decoded.intent_hash().map_err(|error| error.to_string())?;
        Ok(keys.sign_attestation(intent_hash.as_slice()).to_vec())
    })();
    match result {
        Ok(enclave_signature) => EnclaveResponse::RegistrationIntentSignedDevV1 {
            intent,
            enclave_signature,
        },
        Err(message) => EnclaveResponse::Error { message },
    }
}

fn signed_transition_proof(
    keys: &EnclaveKeys,
    intent: &RegistrationIntentV1,
    manifest: &EnclaveInitializationManifestV1,
    resident_offer_public: [u8; 32],
) -> Result<(B256, TransitionKeyReadyProofV1), String> {
    let intent_hash = intent.intent_hash().map_err(|error| error.to_string())?;
    let mut proof = TransitionKeyReadyProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        transition_intent_hash: intent_hash,
        candidate_manifest_hash: manifest
            .authorization_hash()
            .map_err(|error| error.to_string())?,
        transition_nonce: intent.transition_nonce,
        resident_offer_public,
        candidate_attestation_signature: [0; 64],
    };
    let signing_hash = proof.signing_hash().map_err(|error| error.to_string())?;
    proof.candidate_attestation_signature = keys.sign_attestation(signing_hash.as_slice());
    Ok((intent_hash, proof))
}
