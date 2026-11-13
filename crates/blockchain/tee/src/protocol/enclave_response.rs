use alloy_primitives::B256;

// The preserved doc links name `EnclaveRequest`. Rustc does not treat those links as uses.
#[allow(unused_imports)]
use super::enclave_request::EnclaveRequest;
use super::ParticipantAnnounce;

/// Self-observed enclave health snapshot returned by [`EnclaveRequest::Health`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveHealthStatusV1 {
    pub uptime_s: u64,
    pub offer_key_ready: bool,
    pub heap: EnclaveHeapUsageV1,
    pub requests: EnclaveRequestCountsV1,
    pub classes: EnclaveRequestClassCountsV1,
}

/// Enclave heap the allocator accounts for, now and at its peak.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveHeapUsageV1 {
    pub current_bytes: u64,
    pub peak_bytes: u64,
}

/// Requests the enclave dispatched, by outcome.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveRequestCountsV1 {
    pub total: u64,
    pub errored: u64,
    pub denied: u64,
}

/// Requests the enclave dispatched, by authorization class.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveRequestClassCountsV1 {
    pub initialized: u64,
    pub founding_keyless: u64,
    pub keyless_onboarding: u64,
    pub ready: u64,
    pub dev_source_seal: u64,
    pub dev_recipient_ingest: u64,
}

/// EIP-191 `personal_sign` digest of `message`. Matches ethers `signMessage`.
pub fn eip191_hash(message: &[u8]) -> B256 {
    let mut buf = Vec::with_capacity(message.len() + 40);
    buf.extend_from_slice(b"\x19Ethereum Signed Message:\n");
    buf.extend_from_slice(message.len().to_string().as_bytes());
    buf.extend_from_slice(message);
    alloy_primitives::keccak256(buf)
}

/// Infrastructure responses returned from the enclave to the node, plus the
/// opaque [`Self::Payload`] carrier for upper-layer application protocols.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveResponse {
    /// SGX quote bundle.
    Quote {
        mrenclave: B256,
        mrsigner: B256,
        isv_svn: u16,
        report_data: B256,
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
        quote_body: Vec<u8>,
        attestation: String,
    },
    /// Public first-boot material.
    InitializationChallenge {
        challenge: [u8; 32],
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
    },
    /// Intent-bound real quote generated only for an authenticated NodeHost.
    DcapQuote {
        intent: Vec<u8>,
        quote_body: Vec<u8>,
        enclave_signature: Vec<u8>,
        transition_key_ready_proof: Vec<u8>,
    },
    /// Development-only proof of possession over the exact canonical intent.
    RegistrationIntentSignedDevV1 {
        intent: Vec<u8>,
        enclave_signature: Vec<u8>,
    },
    DcapVerificationStartedV1 {
        request_hash: B256,
    },
    DcapVerificationChunkAcceptedV1 {
        request_hash: B256,
        next_offset: u32,
    },
    DcapOnboardingArtifactIngestStartedV1 {
        request_hash: B256,
    },
    DcapOnboardingArtifactChunkAcceptedV1 {
        request_hash: B256,
        next_offset: u32,
    },
    DcapOnboardingArtifactRecordAcceptedV1 {
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
    },
    /// Canonical accepted verdict or stable reject code.
    DcapVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    /// Purpose-bound onboarding verdict plus its deterministic Registry artifact.
    DcapOnboardingVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        onboarding_artifact: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    Handshake {
        noise_msg: Vec<u8>,
    },
    PublicKeys {
        offer_key_ready: bool,
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
        tee_bls_pub: Vec<u8>,
        dkg_enc_pub: [u8; 32],
        dkg_enc_sig: Vec<u8>,
    },
    Initialized {
        enclave_id: B256,
        node_host_authorization_hash: B256,
        sealed_loaded: bool,
    },
    RemoteSessionAuthorizedV1 {
        ticket_id: B256,
    },
    /// Generic acknowledgement (e.g. `DkgOpen` / `DkgDealerReceiveAck`).
    Ack,
    /// Seam A result.
    DkgDealt {
        pub_msg: Vec<u8>,
        sealed_shares: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// Seam B result.
    DkgPlayerAck {
        ack: Option<Vec<u8>>,
    },
    /// Seam D result.
    DkgSignedLog {
        signed_log: Vec<u8>,
    },
    /// Seam E result.
    DkgPlayerFinalized {
        group_public: Vec<u8>,
        share_commitment: B256,
    },
    /// Seam F result.
    DkgNetworkKeyPartial {
        sealed: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// Seam F founding key result.
    DkgNetworkEncryptionKey {
        network_encryption_public_key: [u8; 32],
        group_public_key: Vec<u8>,
    },
    /// One-time onboarding result.
    FinalizedAdmissionIngestedV1 {
        request_hash: B256,
        tribute_offer_public: [u8; 32],
    },
    Error {
        message: String,
    },
    /// Result of [`EnclaveRequest::Health`].
    HealthStatus {
        status: Box<EnclaveHealthStatusV1>,
    },
    /// Ceremony-scoped DKG participant identity.
    DkgParticipantAnnounceV1 {
        participant: ParticipantAnnounce,
    },
    /// Exact purpose-bound artifact produced for a GramineDirectDev registration.
    GramineDirectDevOnboardingArtifactPreparedV1 {
        request_hash: B256,
        onboarding_artifact: Vec<u8>,
    },
    GramineDirectDevOnboardingArtifactIngestedV1 {
        tribute_offer_public: [u8; 32],
    },
    TransitionEvidenceDevV1 {
        evidence: Vec<u8>,
    },
    RemoteSessionsRetiredV1 {
        activation_height: u64,
    },
    UpgradeKeyExportedV1 {
        request_hash: B256,
        artifact: Vec<u8>,
    },
    /// The enclave cannot serve this request yet.
    NotReady {
        message: String,
    },
    /// Signed DirectDev onboarding artifact prepared for one canonical request.
    GramineDirectDevOnboardingArtifactPreparedV2 {
        request_hash: B256,
        onboarding_artifact: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    /// Opaque application-layer response envelope carrying tag-free canonical
    /// payload bytes alongside the witness-tape commitment and local-only signature.
    Payload {
        recordable: bool,
        payload: Vec<u8>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
}

impl core::fmt::Debug for EnclaveResponse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Payload {
                recordable,
                payload,
                inputs_canonical_hash,
                ..
            } => f
                .debug_struct("Payload")
                .field("recordable", recordable)
                .field("payload_len", &payload.len())
                .field("inputs_canonical_hash", inputs_canonical_hash)
                .finish(),
            Self::Error { message } => f.debug_struct("Error").field("message", message).finish(),
            Self::NotReady { message } => f
                .debug_struct("NotReady")
                .field("message", message)
                .finish(),
            _ => f.debug_struct("EnclaveResponse").finish_non_exhaustive(),
        }
    }
}

#[cfg(test)]
mod wire_compat_tests {
    use super::EnclaveResponse;
    use alloy_primitives::B256;

    #[test]
    fn wire_ordinals_stay_pinned() {
        let error = EnclaveResponse::Error {
            message: String::new(),
        };
        let not_ready = EnclaveResponse::NotReady {
            message: String::new(),
        };
        let prepared = EnclaveResponse::GramineDirectDevOnboardingArtifactPreparedV2 {
            request_hash: B256::ZERO,
            onboarding_artifact: Vec::new(),
            attestation_tag: Vec::new(),
        };
        let payload = EnclaveResponse::Payload {
            recordable: true,
            payload: vec![1],
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: Vec::new(),
        };
        assert_eq!(postcard::to_allocvec(&error).unwrap()[0], 23);
        assert_eq!(postcard::to_allocvec(&not_ready).unwrap()[0], 31);
        assert_eq!(postcard::to_allocvec(&prepared).unwrap()[0], 32);
        assert_eq!(postcard::to_allocvec(&payload).unwrap()[0], 33);
    }
}
