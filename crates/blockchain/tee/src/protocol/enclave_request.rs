use alloy_primitives::B256;

use super::{GramineDirectDevOnboardingContextV1, ParticipantAnnounce, PayloadLane};

/// Infrastructure requests sent from the node to the enclave, plus the opaque
/// [`Self::Payload`] carrier for upper-layer application protocols.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveRequest {
    /// Development-only pre-handshake quote. The production server
    /// rejects this variant and never routes it after initialization.
    GetQuote { nonce: [u8; 32] },
    /// Production pre-handshake discovery for an uninitialized enclave.
    GetInitializationChallenge,
    /// Production pre-handshake initialization authorization.
    Initialize {
        manifest: Vec<u8>,
        node_signature: Vec<u8>,
    },
    /// Production pre-handshake marker for an already initialized enclave.
    OpenSession,
    /// Production pre-handshake marker for one previously authorized remote source NodeHost.
    OpenRemoteSessionV1 { ticket_id: B256 },
    /// Noise-IK handshake message.
    SessionHandshake { noise_msg: Vec<u8> },
    /// Return the enclave's public keys.
    GetPublicKeys,
    /// Local-owner command that installs one bounded, one-use remote Noise admission.
    AuthorizeRemoteSessionV1 {
        ticket_id: B256,
        initiator_static_x25519: [u8; 32],
        responder_static_x25519: [u8; 32],
        deadline: u64,
        finalized_block_hash: B256,
    },
    /// Generate a fresh DCAP quote for the exact canonical registration, renewal or transition intent.
    GenerateDcapQuote { intent: Vec<u8> },
    /// Sign one exact GramineDirectDev registration intent inside the enclave.
    SignRegistrationIntentDevV1 { intent: Vec<u8> },
    /// Start one bounded, request-committed DCAP verification upload.
    BeginDcapVerificationV1 {
        request_hash: B256,
        evidence_len: u32,
        policy_len: u32,
        block_timestamp: u64,
    },
    /// Start the dedicated `RegisterEnclave` verify-and-seal flow.
    BeginDcapOnboardingVerificationV1 {
        request_hash: B256,
        evidence_len: u32,
        policy_len: u32,
        block_timestamp: u64,
        node_signature: Vec<u8>,
        enclave_signature: Vec<u8>,
        expected_network_encryption_public: [u8; 32],
        key_epoch: u64,
        network_encryption_epoch: u64,
    },
    /// Append the next exact byte range to the active verification upload.
    DcapVerificationChunkV1 {
        request_hash: B256,
        offset: u32,
        bytes: Vec<u8>,
    },
    /// Finish the exact upload and run the full enclave-resident verifier.
    FinishDcapVerificationV1 { request_hash: B256 },
    /// Open a TEE DKG ceremony session inside the enclave.
    DkgOpen {
        ceremony_id: B256,
        round: u64,
        participants: Vec<ParticipantAnnounce>,
    },
    /// Seam A: deal + seal per-player shares.
    DkgStartDealer { ceremony_id: B256 },
    /// Seam B: open + verify an incoming sealed dealing inside the enclave.
    DkgPlayerIngest {
        ceremony_id: B256,
        dealer_bls: Vec<u8>,
        pub_msg: Vec<u8>,
        sealed_share: Vec<u8>,
    },
    /// Seam C: record a player's acknowledgement at this enclave's dealer.
    DkgDealerReceiveAck {
        ceremony_id: B256,
        player_bls: Vec<u8>,
        ack: Vec<u8>,
    },
    /// Seam D: finalize this enclave's dealing into a signed dealer log.
    DkgDealerFinalize { ceremony_id: B256 },
    /// Seam E: verify collected signed dealer logs and recover this enclave's local threshold share.
    DkgPlayerFinalize {
        ceremony_id: B256,
        signed_logs: Vec<Vec<u8>>,
    },
    /// Seam F (network encryption key): threshold-sign the fixed network-key message and seal partials to recipients.
    DkgNetworkKeyPartial { ceremony_id: B256 },
    /// Founding Seam F: finalize the initial group threshold signature and install the permanent keypair.
    DkgFinalizeNetworkKey {
        ceremony_id: B256,
        sealed_partials: Vec<Vec<u8>>,
        chain_id: B256,
        network_key_epoch: u64,
    },
    /// Start one streaming finalized-admission verification rooted in the measured genesis committee.
    BeginDcapOnboardingArtifactIngestV1 {
        request_hash: B256,
        artifact: Vec<u8>,
        anchor_outcome: Vec<u8>,
        expected_intent_hash: B256,
        expected_network_encryption_public: [u8; 32],
        expected_key_epoch: u64,
        expected_network_encryption_epoch: u64,
    },
    /// Append bytes to the current transition or admission record.
    DcapOnboardingArtifactChunkV1 {
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
        offset: u32,
        bytes: Vec<u8>,
    },
    /// Verify the current complete record.
    CommitDcapOnboardingArtifactRecordV1 {
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
    },
    /// Decrypt, durably seal, and activate the resident offer key after verified admission.
    FinishDcapOnboardingArtifactIngestV1 { request_hash: B256 },
    /// Read-only health/telemetry probe.
    Health,
    /// Produce this initialized enclave's exact ceremony-scoped DKG participant announcement.
    DkgParticipantAnnounceV1 {
        ceremony_id: B256,
        round: u64,
        participant_bls: Vec<Vec<u8>>,
    },
    /// Produce a deterministic offer-key onboarding artifact for one exact GramineDirectDev registration.
    PrepareGramineDirectDevOnboardingArtifactV1 {
        request_hash: B256,
        context: Vec<u8>,
    },
    /// Install an exact finalized DirectDev onboarding artifact into this initialized keyless enclave.
    IngestGramineDirectDevOnboardingArtifactV1 {
        artifact: Vec<u8>,
        expected_intent_hash: B256,
        expected_network_encryption_public: [u8; 32],
        expected_key_epoch: u64,
        expected_network_encryption_epoch: u64,
    },
    /// Prove resident-key readiness without a DCAP quote.
    GenerateTransitionEvidenceDevV1 { intent: Vec<u8> },
    /// Revoke pre-boundary remote tickets and open sessions.
    RetireRemoteSessionsV1 { activation_height: u64 },
    /// Authorize remote session with retirement height.
    AuthorizeRemoteSessionV2 {
        ticket_id: B256,
        initiator_static_x25519: [u8; 32],
        responder_static_x25519: [u8; 32],
        deadline: u64,
        finalized_block_hash: B256,
        retirement_height: u64,
    },
    /// Upgrade transfer using finalized pending-candidate proofs.
    BeginUpgradeKeyTransferV1 {
        request_hash: B256,
        artifact: Vec<u8>,
        anchor_outcome: Vec<u8>,
        export: bool,
    },
    /// Prepare a DirectDev onboarding artifact from the typed canonical context.
    PrepareGramineDirectDevOnboardingArtifactV2 {
        #[serde(with = "canonical_onboarding_context")]
        context: GramineDirectDevOnboardingContextV1,
    },
    /// Opaque application-layer request envelope routed to the upper-layer enclave protocol.
    Payload { lane: PayloadLane, payload: Vec<u8> },
}

mod canonical_onboarding_context {
    use super::GramineDirectDevOnboardingContextV1;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(
        context: &GramineDirectDevOnboardingContextV1,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        context.encode_canonical().serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<GramineDirectDevOnboardingContextV1, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        GramineDirectDevOnboardingContextV1::decode_canonical(&bytes)
            .map_err(|error| serde::de::Error::custom(format!("{error:?}")))
    }
}

impl EnclaveRequest {
    /// Stable snake_case label for this request, used as a metric/log key on
    /// both the node client and the enclave server. Never wire data.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::GetQuote { .. } => "get_quote",
            Self::GetInitializationChallenge => "get_initialization_challenge",
            Self::Initialize { .. } => "initialize",
            Self::OpenSession => "open_session",
            Self::OpenRemoteSessionV1 { .. } => "open_remote_session_v1",
            Self::SessionHandshake { .. } => "session_handshake",
            Self::GetPublicKeys => "get_public_keys",
            Self::AuthorizeRemoteSessionV1 { .. } => "authorize_remote_session_v1",
            Self::GenerateDcapQuote { .. } => "generate_dcap_quote",
            Self::SignRegistrationIntentDevV1 { .. } => "sign_registration_intent_dev_v1",
            Self::BeginDcapVerificationV1 { .. } => "begin_dcap_verification_v1",
            Self::BeginDcapOnboardingVerificationV1 { .. } => {
                "begin_dcap_onboarding_verification_v1"
            }
            Self::DcapVerificationChunkV1 { .. } => "dcap_verification_chunk_v1",
            Self::FinishDcapVerificationV1 { .. } => "finish_dcap_verification_v1",
            Self::DkgOpen { .. } => "dkg_open",
            Self::DkgStartDealer { .. } => "dkg_start_dealer",
            Self::DkgPlayerIngest { .. } => "dkg_player_ingest",
            Self::DkgDealerReceiveAck { .. } => "dkg_dealer_receive_ack",
            Self::DkgDealerFinalize { .. } => "dkg_dealer_finalize",
            Self::DkgPlayerFinalize { .. } => "dkg_player_finalize",
            Self::DkgNetworkKeyPartial { .. } => "dkg_network_key_partial",
            Self::DkgFinalizeNetworkKey { .. } => "dkg_finalize_network_key",
            Self::BeginDcapOnboardingArtifactIngestV1 { .. } => {
                "begin_dcap_onboarding_artifact_ingest_v1"
            }
            Self::DcapOnboardingArtifactChunkV1 { .. } => "dcap_onboarding_artifact_chunk_v1",
            Self::CommitDcapOnboardingArtifactRecordV1 { .. } => {
                "commit_dcap_onboarding_artifact_record_v1"
            }
            Self::FinishDcapOnboardingArtifactIngestV1 { .. } => {
                "finish_dcap_onboarding_artifact_ingest_v1"
            }
            Self::Health => "health",
            Self::DkgParticipantAnnounceV1 { .. } => "dkg_participant_announce_v1",
            Self::PrepareGramineDirectDevOnboardingArtifactV1 { .. } => {
                "prepare_gramine_direct_dev_onboarding_artifact_v1"
            }
            Self::IngestGramineDirectDevOnboardingArtifactV1 { .. } => {
                "ingest_gramine_direct_dev_onboarding_artifact_v1"
            }
            Self::GenerateTransitionEvidenceDevV1 { .. } => "generate_transition_evidence_dev_v1",
            Self::RetireRemoteSessionsV1 { .. } => "retire_remote_sessions_v1",
            Self::AuthorizeRemoteSessionV2 { .. } => "authorize_remote_session_v2",
            Self::BeginUpgradeKeyTransferV1 { .. } => "begin_upgrade_key_transfer_v1",
            Self::PrepareGramineDirectDevOnboardingArtifactV2 { .. } => {
                "prepare_gramine_direct_dev_onboarding_artifact_v2"
            }
            Self::Payload { .. } => "payload",
        }
    }

    /// True when re-sending this request after a lost response cannot
    /// double-apply enclave state. Application-layer [`Self::Payload`] callers
    /// supply explicit [`crate::tee_call::CallMeta`] when overriding retry behavior.
    pub const fn is_idempotent(&self) -> bool {
        match self {
            Self::GetQuote { .. }
            | Self::GetPublicKeys
            | Self::GenerateDcapQuote { .. }
            | Self::SignRegistrationIntentDevV1 { .. }
            | Self::GenerateTransitionEvidenceDevV1 { .. }
            | Self::RetireRemoteSessionsV1 { .. }
            | Self::Health
            | Self::PrepareGramineDirectDevOnboardingArtifactV1 { .. }
            | Self::PrepareGramineDirectDevOnboardingArtifactV2 { .. }
            | Self::Payload { .. } => true,
            Self::BeginUpgradeKeyTransferV1 { .. }
            | Self::GetInitializationChallenge
            | Self::Initialize { .. }
            | Self::OpenSession
            | Self::OpenRemoteSessionV1 { .. }
            | Self::SessionHandshake { .. }
            | Self::AuthorizeRemoteSessionV1 { .. }
            | Self::AuthorizeRemoteSessionV2 { .. }
            | Self::BeginDcapVerificationV1 { .. }
            | Self::BeginDcapOnboardingVerificationV1 { .. }
            | Self::DcapVerificationChunkV1 { .. }
            | Self::FinishDcapVerificationV1 { .. }
            | Self::DkgParticipantAnnounceV1 { .. }
            | Self::DkgOpen { .. }
            | Self::DkgStartDealer { .. }
            | Self::DkgPlayerIngest { .. }
            | Self::DkgDealerReceiveAck { .. }
            | Self::DkgDealerFinalize { .. }
            | Self::DkgPlayerFinalize { .. }
            | Self::DkgNetworkKeyPartial { .. }
            | Self::DkgFinalizeNetworkKey { .. }
            | Self::BeginDcapOnboardingArtifactIngestV1 { .. }
            | Self::DcapOnboardingArtifactChunkV1 { .. }
            | Self::CommitDcapOnboardingArtifactRecordV1 { .. }
            | Self::FinishDcapOnboardingArtifactIngestV1 { .. }
            | Self::IngestGramineDirectDevOnboardingArtifactV1 { .. } => false,
        }
    }

    /// Classify each infrastructure request for consensus witness recording.
    /// Application-layer [`Self::Payload`] recording is governed by the inner
    /// protocol's [`crate::tee_call::CallMeta::recordable`] contract.
    pub const fn consensus_witness_kind(&self) -> super::ConsensusWitnessKind {
        use super::ConsensusWitnessKind::{NonConsensus, Recorded, StreamFrame};
        match self {
            Self::PrepareGramineDirectDevOnboardingArtifactV2 { .. } => Recorded,
            Self::BeginDcapVerificationV1 { .. }
            | Self::BeginDcapOnboardingVerificationV1 { .. }
            | Self::DcapVerificationChunkV1 { .. }
            | Self::FinishDcapVerificationV1 { .. } => StreamFrame,
            Self::GetQuote { .. }
            | Self::GetInitializationChallenge
            | Self::Initialize { .. }
            | Self::OpenSession
            | Self::OpenRemoteSessionV1 { .. }
            | Self::SessionHandshake { .. }
            | Self::GetPublicKeys
            | Self::AuthorizeRemoteSessionV1 { .. }
            | Self::GenerateDcapQuote { .. }
            | Self::SignRegistrationIntentDevV1 { .. }
            | Self::DkgOpen { .. }
            | Self::DkgStartDealer { .. }
            | Self::DkgPlayerIngest { .. }
            | Self::DkgDealerReceiveAck { .. }
            | Self::DkgDealerFinalize { .. }
            | Self::DkgPlayerFinalize { .. }
            | Self::DkgNetworkKeyPartial { .. }
            | Self::DkgFinalizeNetworkKey { .. }
            | Self::BeginDcapOnboardingArtifactIngestV1 { .. }
            | Self::DcapOnboardingArtifactChunkV1 { .. }
            | Self::CommitDcapOnboardingArtifactRecordV1 { .. }
            | Self::FinishDcapOnboardingArtifactIngestV1 { .. }
            | Self::Health
            | Self::DkgParticipantAnnounceV1 { .. }
            | Self::PrepareGramineDirectDevOnboardingArtifactV1 { .. }
            | Self::IngestGramineDirectDevOnboardingArtifactV1 { .. }
            | Self::GenerateTransitionEvidenceDevV1 { .. }
            | Self::RetireRemoteSessionsV1 { .. }
            | Self::AuthorizeRemoteSessionV2 { .. }
            | Self::BeginUpgradeKeyTransferV1 { .. }
            | Self::Payload { .. } => NonConsensus,
        }
    }
}

impl core::fmt::Debug for EnclaveRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Payload { lane, payload } => f
                .debug_struct("Payload")
                .field("lane", lane)
                .field("payload_len", &payload.len())
                .finish(),
            other => f
                .debug_struct("EnclaveRequest")
                .field("label", &other.label())
                .finish_non_exhaustive(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ConsensusWitnessKind;

    fn onboarding_context() -> GramineDirectDevOnboardingContextV1 {
        GramineDirectDevOnboardingContextV1 {
            chain_id: [1; 32],
            genesis_hash: B256::repeat_byte(2),
            intent_hash: B256::repeat_byte(3),
            node_id_hash: B256::repeat_byte(4),
            enclave_id: B256::repeat_byte(5),
            binding_id: B256::repeat_byte(6),
            policy_hash: B256::repeat_byte(7),
            recipient_x25519: [8; 32],
            network_encryption_public: [9; 32],
            key_epoch: 10,
            network_encryption_epoch: 11,
        }
    }

    #[test]
    fn wire_ordinals_and_canonical_context_round_trip() {
        let legacy = EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV1 {
            request_hash: B256::ZERO,
            context: vec![],
        };
        let v2 = EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV2 {
            context: onboarding_context(),
        };
        let payload = EnclaveRequest::Payload {
            lane: PayloadLane::Execution,
            payload: vec![1, 2, 3],
        };
        assert_eq!(postcard::to_allocvec(&legacy).unwrap()[0], 28);
        assert_eq!(postcard::to_allocvec(&v2).unwrap()[0], 34);
        assert_eq!(postcard::to_allocvec(&payload).unwrap()[0], 35);
        let encoded = postcard::to_allocvec(&v2).unwrap();
        assert_eq!(
            postcard::from_bytes::<EnclaveRequest>(&encoded).unwrap(),
            v2
        );
    }

    #[test]
    fn consensus_witness_classifier_marks_recorded_stream_and_non_consensus_requests() {
        let stream = EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: B256::ZERO,
            offset: 0,
            bytes: vec![],
        };
        let non_consensus = EnclaveRequest::Health;
        assert_eq!(
            stream.consensus_witness_kind(),
            ConsensusWitnessKind::StreamFrame
        );
        assert_eq!(
            non_consensus.consensus_witness_kind(),
            ConsensusWitnessKind::NonConsensus
        );
        assert_eq!(
            EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV2 {
                context: onboarding_context(),
            }
            .consensus_witness_kind(),
            ConsensusWitnessKind::Recorded
        );
    }
}
