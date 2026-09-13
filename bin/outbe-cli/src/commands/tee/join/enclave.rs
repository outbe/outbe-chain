use alloy_primitives::B256;

use eyre::Result;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use outbe_tee::finalized_admission::FinalizedAdmissionRecordKindV1;

use outbe_tee::protocol::EnclaveRequest;
use outbe_tee::protocol::EnclaveResponse;
use outbe_tee::AuthorizedEnclaveClient;

use outbe_tee::EnclaveClient;

use outbe_tee::GeneratedDcapQuoteV1;

use outbe_tee::ReplacementCandidateEnclaveV1;

use outbe_tee::TransportError;

pub(in super::super) enum JoinEnclave {
    Committed(AuthorizedEnclaveClient),
    Candidate(Box<ReplacementCandidateEnclaveV1>),
    Development(Box<EnclaveClient>),
}

impl JoinEnclave {
    pub(in super::super) fn is_candidate(&self) -> bool {
        matches!(self, Self::Candidate(_))
    }

    pub(in super::super) fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1> {
        match self {
            Self::Committed(client) => client.generate_dcap_quote(intent),
            Self::Candidate(client) => client.generate_dcap_quote(intent),
            Self::Development(_) => unreachable!("DCAP join cannot use development transport"),
        }
        .map_err(|error| eyre::eyre!("generate intent-bound DCAP quote: {error}"))
    }

    pub(in super::super) fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        match self {
            Self::Committed(client) => client.sign_registration_intent_dev_v1(intent),
            Self::Candidate(client) => client.sign_registration_intent_dev_v1(intent),
            Self::Development(client) => client.sign_registration_intent_dev_v1(intent),
        }
        .map_err(|error| eyre::eyre!("sign development V1 intent: {error}"))
    }

    pub(in super::super) fn request(
        &mut self,
        request: &EnclaveRequest,
    ) -> Result<EnclaveResponse> {
        self.request_transport(request)
            .map_err(|error| eyre::eyre!(error))
    }

    pub(in super::super) fn request_transport(
        &mut self,
        request: &EnclaveRequest,
    ) -> std::result::Result<EnclaveResponse, TransportError> {
        match self {
            Self::Committed(client) => client.request(request),
            Self::Candidate(client) => client.request(request),
            Self::Development(client) => client.request(request),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn begin_finalized_admission_v1(
        &mut self,
        artifact: &[u8],
        anchor_outcome: &[u8],
        expected_intent_hash: B256,
        expected_tribute_offer_public: [u8; 32],
        expected_key_epoch: u64,
        expected_tribute_offer_epoch: u64,
    ) -> std::result::Result<B256, TransportError> {
        match self {
            Self::Committed(client) => client.begin_finalized_admission_v1(
                artifact,
                anchor_outcome,
                expected_intent_hash,
                expected_tribute_offer_public,
                expected_key_epoch,
                expected_tribute_offer_epoch,
            ),
            Self::Candidate(client) => client.begin_finalized_admission_v1(
                artifact,
                anchor_outcome,
                expected_intent_hash,
                expected_tribute_offer_public,
                expected_key_epoch,
                expected_tribute_offer_epoch,
            ),
            Self::Development(_) => {
                unreachable!("finalized admission cannot use development transport")
            }
        }
    }

    pub(in super::super) fn upload_finalized_admission_record_v1(
        &mut self,
        request_hash: B256,
        kind: FinalizedAdmissionRecordKindV1,
        record: &[u8],
    ) -> std::result::Result<(), TransportError> {
        match self {
            Self::Committed(client) => {
                client.upload_finalized_admission_record_v1(request_hash, kind, record)
            }
            Self::Candidate(client) => {
                client.upload_finalized_admission_record_v1(request_hash, kind, record)
            }
            Self::Development(_) => {
                unreachable!("finalized admission cannot use development transport")
            }
        }
    }

    pub(in super::super) fn finish_finalized_admission_v1(
        &mut self,
        request_hash: B256,
        expected_tribute_offer_public: [u8; 32],
    ) -> std::result::Result<[u8; 32], TransportError> {
        match self {
            Self::Committed(client) => {
                client.finish_finalized_admission_v1(request_hash, expected_tribute_offer_public)
            }
            Self::Candidate(client) => {
                client.finish_finalized_admission_v1(request_hash, expected_tribute_offer_public)
            }
            Self::Development(_) => {
                unreachable!("finalized admission cannot use development transport")
            }
        }
    }
}
