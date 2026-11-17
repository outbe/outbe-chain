//! Bounded, per-Noise-session upload for enclave-resident DCAP verification.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{MAX_ATTESTATION_EVIDENCE_BYTES, MAX_TEE_POLICY_BYTES};
use outbe_tee::{
    dcap_protocol::{
        dcap_onboarding_attestation_preimage, dcap_onboarding_request_hash,
        dcap_verification_attestation_preimage, dcap_verification_request_hash,
        DcapOnboardingArtifactV1, DcapOnboardingContextV1, DcapRejectCodeV1,
        DcapVerificationOutcomeV1, RegistrationVerificationRequest,
        MAX_DCAP_VERIFICATION_CHUNK_BYTES,
    },
    protocol::{EnclaveRequest, EnclaveResponse},
};

use crate::{
    errors::{DcapVerifyError, EnclaveResult},
    keys::EnclaveKeys,
    transport::DerivedTributeOfferKey,
};

#[derive(Debug)]
struct UploadV1 {
    request_hash: B256,
    evidence_len: usize,
    policy_len: usize,
    block_timestamp: u64,
    purpose: VerificationPurposeV1,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VerificationPurposeV1 {
    Generic,
    RegisterOnboarding {
        node_signature: [u8; 65],
        enclave_signature: [u8; 64],
        expected_tribute_offer_public: [u8; 32],
        key_epoch: u64,
        tribute_offer_epoch: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CompleteDcapVerificationV1 {
    request_hash: B256,
    evidence: Vec<u8>,
    policy: Vec<u8>,
    block_timestamp: u64,
    pub(crate) purpose: VerificationPurposeV1,
}

pub(crate) enum DcapVerificationProgressV1 {
    Started {
        request_hash: B256,
    },
    ChunkAccepted {
        request_hash: B256,
        next_offset: u32,
    },
    Complete(Box<CompleteDcapVerificationV1>),
}

/// One bounded upload at a time on one authenticated Noise connection.
#[derive(Default)]
pub(crate) struct DcapVerificationSessionV1 {
    upload: Option<UploadV1>,
}

impl DcapVerificationSessionV1 {
    pub(crate) fn handle(
        &mut self,
        request: EnclaveRequest,
    ) -> EnclaveResult<DcapVerificationProgressV1> {
        let result = match request {
            EnclaveRequest::BeginDcapVerificationV1 {
                request_hash,
                evidence_len,
                policy_len,
                block_timestamp,
            } => self.begin(
                request_hash,
                evidence_len,
                policy_len,
                block_timestamp,
                VerificationPurposeV1::Generic,
            ),
            EnclaveRequest::BeginDcapOnboardingVerificationV1 {
                request_hash,
                evidence_len,
                policy_len,
                block_timestamp,
                node_signature,
                enclave_signature,
                expected_network_encryption_public,
                key_epoch,
                network_encryption_epoch,
            } => {
                let node_signature = node_signature
                    .try_into()
                    .map_err(|_| DcapVerifyError::OnboardingNodeSignatureLen)?;
                let enclave_signature = enclave_signature
                    .try_into()
                    .map_err(|_| DcapVerifyError::OnboardingEnclaveSignatureLen)?;
                self.begin(
                    request_hash,
                    evidence_len,
                    policy_len,
                    block_timestamp,
                    VerificationPurposeV1::RegisterOnboarding {
                        node_signature,
                        enclave_signature,
                        expected_tribute_offer_public: expected_network_encryption_public,
                        key_epoch,
                        tribute_offer_epoch: network_encryption_epoch,
                    },
                )
            }
            EnclaveRequest::DcapVerificationChunkV1 {
                request_hash,
                offset,
                bytes,
            } => self.push(request_hash, offset, bytes),
            EnclaveRequest::FinishDcapVerificationV1 { request_hash } => self.finish(request_hash),
            _ => Err(DcapVerifyError::NotUploadRequest.into()),
        };
        if result.is_err() {
            self.upload = None;
        }
        result
    }

    fn begin(
        &mut self,
        request_hash: B256,
        evidence_len: u32,
        policy_len: u32,
        block_timestamp: u64,
        purpose: VerificationPurposeV1,
    ) -> EnclaveResult<DcapVerificationProgressV1> {
        if self.upload.is_some() {
            return Err(DcapVerifyError::UploadAlreadyActive.into());
        }
        let evidence_len =
            usize::try_from(evidence_len).map_err(|_| DcapVerifyError::EvidenceLenTarget)?;
        let policy_len =
            usize::try_from(policy_len).map_err(|_| DcapVerifyError::PolicyLenTarget)?;
        if evidence_len > MAX_ATTESTATION_EVIDENCE_BYTES {
            return Err(DcapVerifyError::EvidenceLenCap.into());
        }
        if policy_len > MAX_TEE_POLICY_BYTES {
            return Err(DcapVerifyError::PolicyLenCap.into());
        }
        let total = evidence_len
            .checked_add(policy_len)
            .ok_or(DcapVerifyError::UploadLenOverflow)?;
        self.upload = Some(UploadV1 {
            request_hash,
            evidence_len,
            policy_len,
            block_timestamp,
            purpose,
            bytes: Vec::with_capacity(total),
        });
        Ok(DcapVerificationProgressV1::Started { request_hash })
    }

    fn push(
        &mut self,
        request_hash: B256,
        offset: u32,
        bytes: Vec<u8>,
    ) -> EnclaveResult<DcapVerificationProgressV1> {
        let upload = self
            .upload
            .as_mut()
            .ok_or(DcapVerifyError::NoUploadActive)?;
        if upload.request_hash != request_hash {
            return Err(DcapVerifyError::ChunkHashMismatch.into());
        }
        if bytes.is_empty() || bytes.len() > MAX_DCAP_VERIFICATION_CHUNK_BYTES {
            return Err(DcapVerifyError::ChunkLenInvalid.into());
        }
        let offset = usize::try_from(offset).map_err(|_| DcapVerifyError::OffsetOverflow)?;
        if offset != upload.bytes.len() {
            return Err(DcapVerifyError::ChunkOffsetNonSequential.into());
        }
        let total = upload
            .evidence_len
            .checked_add(upload.policy_len)
            .ok_or(DcapVerifyError::UploadLenOverflow)?;
        let next = offset
            .checked_add(bytes.len())
            .ok_or(DcapVerifyError::ChunkLenOverflow)?;
        if next > total {
            return Err(DcapVerifyError::ChunkExceedsDeclaredLen.into());
        }
        upload.bytes.extend_from_slice(&bytes);
        let next_offset = u32::try_from(next).map_err(|_| DcapVerifyError::OffsetOverflow)?;
        Ok(DcapVerificationProgressV1::ChunkAccepted {
            request_hash,
            next_offset,
        })
    }

    fn finish(&mut self, request_hash: B256) -> EnclaveResult<DcapVerificationProgressV1> {
        let upload = self.upload.take().ok_or(DcapVerifyError::NoUploadActive)?;
        if upload.request_hash != request_hash {
            return Err(DcapVerifyError::FinishHashMismatch.into());
        }
        let total = upload
            .evidence_len
            .checked_add(upload.policy_len)
            .ok_or(DcapVerifyError::UploadLenOverflow)?;
        if upload.bytes.len() != total {
            return Err(DcapVerifyError::UploadTruncated.into());
        }
        let policy = upload.bytes[upload.evidence_len..].to_vec();
        let evidence = upload.bytes[..upload.evidence_len].to_vec();
        let computed = match upload.purpose {
            VerificationPurposeV1::Generic => {
                dcap_verification_request_hash(&evidence, &policy, upload.block_timestamp)
            }
            VerificationPurposeV1::RegisterOnboarding {
                ref node_signature,
                ref enclave_signature,
                expected_tribute_offer_public,
                key_epoch,
                tribute_offer_epoch,
            } => dcap_onboarding_request_hash(RegistrationVerificationRequest {
                evidence: &evidence,
                policy: &policy,
                block_timestamp: upload.block_timestamp,
                node_signature,
                enclave_signature,
                expected_network_encryption_public: expected_tribute_offer_public,
                key_epoch,
                network_encryption_epoch: tribute_offer_epoch,
            }),
        }
        .map_err(|_| DcapVerifyError::CommitmentInvalid)?;
        if computed != request_hash {
            return Err(DcapVerifyError::CommitmentMismatch.into());
        }
        Ok(DcapVerificationProgressV1::Complete(Box::new(
            CompleteDcapVerificationV1 {
                request_hash,
                evidence,
                policy,
                block_timestamp: upload.block_timestamp,
                purpose: upload.purpose,
            },
        )))
    }
}

pub(crate) fn complete_verification_response(
    request: CompleteDcapVerificationV1,
    keys: &EnclaveKeys,
    resident_offer_key: Option<&DerivedTributeOfferKey>,
    source_manifest: Option<&outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1>,
) -> EnclaveResponse {
    #[cfg(feature = "native-dcap")]
    let outcome = Ok(outbe_tee::verifier::verify_complete_request(
        &request.evidence,
        &request.policy,
        request.block_timestamp,
    ));
    #[cfg(not(feature = "native-dcap"))]
    let outcome: EnclaveResult<DcapVerificationOutcomeV1> =
        Err(DcapVerifyError::MissingNativeDcapVerifier.into());

    outcome
        .and_then(|outcome| {
            prepare_verification_response(
                &request,
                outcome,
                keys,
                resident_offer_key,
                source_manifest,
            )
        })
        .unwrap_or_else(|message| EnclaveResponse::Error {
            message: message.to_string(),
        })
}

fn prepare_verification_response(
    request: &CompleteDcapVerificationV1,
    mut outcome: DcapVerificationOutcomeV1,
    keys: &EnclaveKeys,
    resident_offer_key: Option<&DerivedTributeOfferKey>,
    source_manifest: Option<&outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1>,
) -> EnclaveResult<EnclaveResponse> {
    let onboarding = matches!(
        request.purpose,
        VerificationPurposeV1::RegisterOnboarding { .. }
    );
    let artifact = if onboarding {
        build_onboarding_artifact(
            request,
            &mut outcome,
            resident_offer_key,
            source_manifest,
            keys.code_identity(),
        )?
    } else {
        None
    };
    let outcome = outcome
        .encode_canonical()
        .map_err(|_| DcapVerifyError::NonCanonicalOutcome)?;
    if onboarding {
        let onboarding_artifact = artifact
            .map(|artifact| artifact.encode_canonical())
            .transpose()
            .map_err(|_| DcapVerifyError::NonCanonicalOnboardingArtifact)?
            .unwrap_or_default();
        let preimage = dcap_onboarding_attestation_preimage(
            request.request_hash,
            &outcome,
            &onboarding_artifact,
        )
        .map_err(|_| DcapVerifyError::OversizedOnboardingResult)?;
        return Ok(EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
            request_hash: request.request_hash,
            outcome,
            onboarding_artifact,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        });
    }
    let preimage = dcap_verification_attestation_preimage(request.request_hash, &outcome)
        .map_err(|_| DcapVerifyError::OversizedOutcome)?;
    Ok(EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash: request.request_hash,
        outcome,
        attestation_tag: keys.sign_attestation(&preimage).to_vec(),
    })
}

/// Build a deterministic purpose-bound artifact after TeeRegistry has already
/// validated the DirectDev registration. The request carries only the exact
/// canonical context, never host-selected plaintext key material.
pub(crate) fn complete_gramine_direct_dev_onboarding_response(
    request_hash: B256,
    context: &[u8],
    resident_offer_key: Option<&DerivedTributeOfferKey>,
    source_manifest: Option<&outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1>,
) -> EnclaveResponse {
    let context = match DcapOnboardingContextV1::decode_canonical(context) {
        Ok(context) => context,
        Err(_) => {
            return EnclaveResponse::Error {
                message: "NoAttestationDev onboarding context is not canonical".into(),
            }
        }
    };
    if request_hash != context.context_hash() {
        return EnclaveResponse::Error {
            message: "NoAttestationDev onboarding context hash mismatch".into(),
        };
    }
    let artifact = match build_gramine_direct_dev_onboarding_artifact(
        context,
        resident_offer_key,
        source_manifest,
    ) {
        Ok(artifact) => artifact,
        Err(message) => {
            return EnclaveResponse::Error {
                message: message.to_string(),
            };
        }
    };
    let onboarding_artifact = match artifact.encode_canonical() {
        Ok(artifact) => artifact,
        Err(_) => {
            return EnclaveResponse::Error {
                message: "enclave produced a non-canonical NoAttestationDev onboarding artifact"
                    .to_string(),
            };
        }
    };
    EnclaveResponse::NoAttestationDevOnboardingArtifactPreparedV1 {
        request_hash,
        onboarding_artifact,
    }
}

pub(crate) fn complete_gramine_direct_dev_onboarding_response_v2(
    context: DcapOnboardingContextV1,
    resident_offer_key: Option<&DerivedTributeOfferKey>,
    source_manifest: Option<&outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1>,
    keys: &EnclaveKeys,
) -> EnclaveResponse {
    let request_hash = context.context_hash();
    let artifact = match build_gramine_direct_dev_onboarding_artifact(
        context,
        resident_offer_key,
        source_manifest,
    ) {
        Ok(artifact) => artifact,
        Err(message) => {
            return EnclaveResponse::Error {
                message: message.to_string(),
            };
        }
    };
    let onboarding_artifact = match artifact.encode_canonical() {
        Ok(artifact) => artifact,
        Err(_) => {
            return EnclaveResponse::Error {
                message: "enclave produced a non-canonical NoAttestationDev onboarding artifact"
                    .to_string(),
            };
        }
    };
    let preimage = match outbe_tee::tee_call::canonical_attestation_preimage(
        b"outbe/tee/gramine-direct-dev-onboarding-result/v2",
        request_hash,
        &onboarding_artifact,
    ) {
        Ok(preimage) => preimage,
        Err(error) => {
            return EnclaveResponse::Error {
                message: error.to_string(),
            }
        }
    };
    EnclaveResponse::NoAttestationDevOnboardingArtifactPreparedV2 {
        request_hash,
        onboarding_artifact,
        attestation_tag: keys.sign_attestation(&preimage).to_vec(),
    }
}

fn build_gramine_direct_dev_onboarding_artifact(
    context: DcapOnboardingContextV1,
    resident_offer_key: Option<&DerivedTributeOfferKey>,
    source_manifest: Option<&outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1>,
) -> EnclaveResult<DcapOnboardingArtifactV1> {
    use outbe_primitives::tee_attestation_v1::AttestationMode;

    let source_manifest = source_manifest.ok_or(DcapVerifyError::OnboardingSourceNotInitialized)?;
    if source_manifest.attestation_mode != AttestationMode::NoAttestationDev
        || source_manifest.chain_id != context.chain_id
        || source_manifest.genesis_hash != context.genesis_hash
    {
        return Err(DcapVerifyError::DirectDevSourceNetworkMismatch.into());
    }
    let resident_offer_key =
        resident_offer_key.ok_or(DcapVerifyError::OnboardingSourceOfferKeyNotReady)?;
    if resident_offer_key.public() != context.network_encryption_public
        || resident_offer_key.key_epoch() != context.key_epoch
        || resident_offer_key.tribute_offer_epoch() != context.network_encryption_epoch
    {
        return Err(DcapVerifyError::DirectDevOfferKeyContextMismatch.into());
    }
    crate::crypto::encrypt_onboarding_artifact_v1(
        resident_offer_key.secret(),
        context,
        resident_offer_key.group_sig(),
    )
    .map_err(|_| DcapVerifyError::DirectDevArtifactEncryptionFailed.into())
}

fn build_onboarding_artifact(
    request: &CompleteDcapVerificationV1,
    outcome: &mut DcapVerificationOutcomeV1,
    resident_offer_key: Option<&DerivedTributeOfferKey>,
    source_manifest: Option<&outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1>,
    source_code_identity: (B256, B256, u16, u16),
) -> EnclaveResult<Option<outbe_tee::dcap_protocol::DcapOnboardingArtifactV1>> {
    if !matches!(outcome, DcapVerificationOutcomeV1::Accepted(_)) {
        return Ok(None);
    }
    let VerificationPurposeV1::RegisterOnboarding {
        node_signature,
        enclave_signature,
        expected_tribute_offer_public,
        key_epoch,
        tribute_offer_epoch,
    } = &request.purpose
    else {
        return Ok(None);
    };
    let evidence = match outbe_tee::verifier::validate_onboarding_registration(
        &request.evidence,
        node_signature,
        enclave_signature,
    ) {
        Ok(evidence) => evidence,
        Err(code) => {
            *outcome = DcapVerificationOutcomeV1::Rejected(code);
            return Ok(None);
        }
    };
    let intent = &evidence.intent;
    let source_manifest = source_manifest.ok_or(DcapVerifyError::OnboardingSourceNotInitialized)?;
    let resident_offer_key =
        resident_offer_key.ok_or(DcapVerifyError::OnboardingSourceOfferKeyNotReady)?;
    let DcapVerificationOutcomeV1::Accepted(verdict) = outcome else {
        unreachable!("non-accepted outcomes return before onboarding construction")
    };
    if source_manifest.attestation_mode
        != outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired
        || (
            verdict.mrenclave,
            verdict.mrsigner,
            verdict.isv_prod_id,
            verdict.isv_svn,
        ) != source_code_identity
    {
        *outcome = DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::MeasurementRejected);
        return Ok(None);
    }
    if !onboarding_context_matches(
        intent,
        source_manifest,
        resident_offer_key,
        &request.purpose,
    ) {
        *outcome = DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::OnboardingContextMismatch);
        return Ok(None);
    }
    let context = DcapOnboardingContextV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        intent_hash: intent
            .intent_hash()
            .map_err(|_| DcapVerifyError::OnboardingIntentHashUnavailable)?,
        node_id_hash: intent
            .node_id
            .node_id_hash()
            .map_err(|_| DcapVerifyError::OnboardingNodeIdHashUnavailable)?,
        enclave_id: intent
            .derived_enclave_id()
            .map_err(|_| DcapVerifyError::OnboardingEnclaveIdUnavailable)?,
        binding_id: intent.binding_id,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519,
        network_encryption_public: *expected_tribute_offer_public,
        key_epoch: *key_epoch,
        network_encryption_epoch: *tribute_offer_epoch,
    };
    crate::crypto::encrypt_onboarding_artifact_v1(
        resident_offer_key.secret(),
        context,
        resident_offer_key.group_sig(),
    )
    .map(Some)
    .map_err(|_| DcapVerifyError::ArtifactEncryptionFailed.into())
}

fn onboarding_context_matches(
    intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    manifest: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    offer_key: &DerivedTributeOfferKey,
    purpose: &VerificationPurposeV1,
) -> bool {
    if intent.chain_id != manifest.chain_id || intent.genesis_hash != manifest.genesis_hash {
        return false;
    }
    match purpose {
        VerificationPurposeV1::RegisterOnboarding {
            expected_tribute_offer_public,
            key_epoch,
            tribute_offer_epoch,
            ..
        } => {
            offer_key.public() == *expected_tribute_offer_public
                && offer_key.key_epoch() == *key_epoch
                && offer_key.tribute_offer_epoch() == *tribute_offer_epoch
        }
        VerificationPurposeV1::Generic => false,
    }
}

#[cfg(test)]
mod tests;
