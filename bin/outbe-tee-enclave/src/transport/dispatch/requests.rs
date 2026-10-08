use crate::transport::*;

/// Dispatch a post-handshake request to a response. `offer_key` is the shared
/// DKG-derived offer key slot: once Seam F populates it, the offer-decrypt path
/// and `GetPublicKeys` use it instead of the pre-DKG dev offer key.
pub fn dispatch(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    chain_id: alloy_primitives::B256,
) -> EnclaveResponse {
    dispatch_with_initialization(
        req,
        keys,
        dkg,
        offer_key,
        DispatchInitializationContext {
            chain_id,
            boot: None,
            initialization: None,
            quote_generator: crate::gramine::dcap_quote,
        },
    )
}

type QuoteGenerator = fn(&[u8; 64]) -> Result<Vec<u8>, String>;

#[derive(Clone, Copy)]
pub(in crate::transport) struct DispatchInitializationContext<'a> {
    pub(in crate::transport) chain_id: B256,
    pub(in crate::transport) boot: Option<&'a EnclaveBootConfig>,
    pub(in crate::transport) initialization: Option<&'a InitializationState>,
    pub(in crate::transport) quote_generator: QuoteGenerator,
}

#[derive(Clone, Copy)]
pub(super) struct RequestContext<'a> {
    pub(super) keys: &'a EnclaveKeys,
    pub(super) offer_key: &'a SharedTributeOfferKey,
    pub(super) chain_id: B256,
    pub(super) initialization: DispatchInitializationContext<'a>,
}

pub(in crate::transport) fn dispatch_with_initialization(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    context: DispatchInitializationContext<'_>,
) -> EnclaveResponse {
    let chain_id = context.chain_id;
    let resident = RequestContext {
        keys,
        offer_key,
        chain_id,
        initialization: context,
    };
    match req {
        req @ (EnclaveRequest::CreateNodForTestV2 { .. }
        | EnclaveRequest::PrepareEncryptedNodsV2 { .. }
        | EnclaveRequest::OpenEncryptedNodsV2 { .. }
        | EnclaveRequest::MineEncryptedNodV2 { .. }
        | EnclaveRequest::ReadNodAmountV2 { .. }
        | EnclaveRequest::NodTransferChunkV2 { .. }
        | EnclaveRequest::ExecuteNodTransferV2 { .. }
        | EnclaveRequest::ReadNodTransferV2 { .. }
        | EnclaveRequest::DiscardNodTransferV2 { .. }) => super::nod_requests::dispatch(req, resident),
        req @ (EnclaveRequest::ApplyTributeDayOpV2 { .. }
        | EnclaveRequest::ReadTributeDayAmountV2 { .. }
        | EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { .. }
        | EnclaveRequest::ReadTributeAmountsV2 { .. }
        | EnclaveRequest::ProcessTributeOfferBatch { .. }) => super::tribute_requests::dispatch(req, resident),
        EnclaveRequest::GetQuote { .. }
        | EnclaveRequest::GetInitializationChallenge
        | EnclaveRequest::Initialize { .. }
        | EnclaveRequest::OpenSession
        | EnclaveRequest::OpenRemoteSessionV1 { .. }
        | EnclaveRequest::SessionHandshake { .. } => EnclaveResponse::Error {
            message: "pre-handshake request is not valid inside a Noise session".to_string(),
        },
        EnclaveRequest::BeginDcapVerificationV1 { .. }
        | EnclaveRequest::BeginDcapOnboardingVerificationV1 { .. }
        | EnclaveRequest::DcapVerificationChunkV1 { .. }
        | EnclaveRequest::FinishDcapVerificationV1 { .. } => EnclaveResponse::Error {
            message: "DCAP verification requires an authenticated production session".to_string(),
        },
        EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV1 { .. } => {
            EnclaveResponse::Error {
                message: "GramineDirectDev artifact creation requires an authenticated production session"
                    .to_string(),
            }
        }
        req @ (EnclaveRequest::AuthorizeRemoteSessionV2 { .. }
        | EnclaveRequest::AuthorizeRemoteSessionV1 { .. }
        | EnclaveRequest::RetireRemoteSessionsV1 { .. }) => super::remote_sessions::dispatch(req, resident),
        req @ (EnclaveRequest::Health
        | EnclaveRequest::GetPublicKeys) => super::queries::dispatch(req, resident),
        EnclaveRequest::IngestGramineDirectDevOnboardingArtifactV1 {
            artifact,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
        } => complete_gramine_direct_dev_onboarding_ingest_response(
            &artifact,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
            keys,
            offer_key,
            context.boot,
            context.initialization,
        ),
        req @ (EnclaveRequest::GenerateDcapQuote { .. }
        | EnclaveRequest::GenerateTransitionEvidenceDevV1 { .. }
        | EnclaveRequest::SignRegistrationIntentDevV1 { .. }) => super::registration::dispatch(req, resident),
        req @ (EnclaveRequest::ApplyGratisOp { .. }
        | EnclaveRequest::ApplyFidelityCohortOp { .. }
        | EnclaveRequest::SnapshotFidelityLeagues { .. }
        | EnclaveRequest::QueryFidelityIndex { .. }
        | EnclaveRequest::ApplyPromisOp { .. }
        | EnclaveRequest::DeriveAccountKeys { .. }) => super::ledgers::dispatch(req, resident),
        EnclaveRequest::BeginUpgradeKeyTransferV1 { .. }
        | EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
        | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
        | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
        | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. } => EnclaveResponse::Error {
            message: "onboarding artifact ingest requires an authenticated production session"
                .into(),
        },
        req @ (EnclaveRequest::DkgParticipantAnnounceV1 { .. }
        | EnclaveRequest::DkgOpen { .. }
        | EnclaveRequest::DkgStartDealer { .. }
        | EnclaveRequest::DkgPlayerIngest { .. }
        | EnclaveRequest::DkgDealerReceiveAck { .. }
        | EnclaveRequest::DkgDealerFinalize { .. }
        | EnclaveRequest::DkgPlayerFinalize { .. }
        | EnclaveRequest::DkgTributeOfferPartial { .. }
        | EnclaveRequest::DkgFinalizeTributeOffer { .. }) => super::dkg_requests::dispatch(req, resident, dkg),

    }
}

/// Map a seam `Result` into an `EnclaveResponse`, turning errors into the typed
/// `Error` response the host surfaces (never a panic).
pub(in crate::transport) fn into_response(
    result: crate::errors::Result<EnclaveResponse>,
) -> EnclaveResponse {
    result.unwrap_or_else(|e| EnclaveResponse::Error {
        message: e.to_string(),
    })
}
