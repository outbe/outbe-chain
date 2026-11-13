use outbe_enclave::protocol::{EnclaveRequest as DomainRequest, EnclaveResponse as DomainResponse};
use outbe_tee::protocol::PayloadLane;

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

/// Dispatch an application-domain request directly against resident enclave keys.
pub fn dispatch_domain(
    req: DomainRequest,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain_id: alloy_primitives::B256,
) -> DomainResponse {
    dispatch_domain_with_context(
        req,
        RequestContext {
            keys,
            offer_key,
            chain_id,
            initialization: DispatchInitializationContext {
                chain_id,
                boot: None,
                initialization: None,
                quote_generator: crate::gramine::dcap_quote,
            },
        },
    )
}

type QuoteGenerator = fn(&[u8; 64]) -> EnclaveResult<Vec<u8>>;

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
    dispatch_with_label(req, keys, dkg, offer_key, context).0
}

pub(in crate::transport) fn dispatch_with_label(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    context: DispatchInitializationContext<'_>,
) -> (EnclaveResponse, Option<&'static str>) {
    let chain_id = context.chain_id;
    let resident = RequestContext {
        keys,
        offer_key,
        chain_id,
        initialization: context,
    };
    match req {
        EnclaveRequest::Payload { lane, payload } => dispatch_payload(lane, &payload, resident),
        EnclaveRequest::GetQuote { .. }
        | EnclaveRequest::GetInitializationChallenge
        | EnclaveRequest::Initialize { .. }
        | EnclaveRequest::OpenSession
        | EnclaveRequest::OpenRemoteSessionV1 { .. }
        | EnclaveRequest::SessionHandshake { .. } => (
            EnclaveResponse::Error {
                message: "pre-handshake request is not valid inside a Noise session".to_string(),
            },
            None,
        ),
        EnclaveRequest::BeginDcapVerificationV1 { .. }
        | EnclaveRequest::BeginDcapOnboardingVerificationV1 { .. }
        | EnclaveRequest::DcapVerificationChunkV1 { .. }
        | EnclaveRequest::FinishDcapVerificationV1 { .. } => (
            EnclaveResponse::Error {
                message: "DCAP verification requires an authenticated production session"
                    .to_string(),
            },
            None,
        ),
        EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV1 { .. }
        | EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV2 { .. } => (
            EnclaveResponse::Error {
                message:
                    "GramineDirectDev artifact creation requires an authenticated production session"
                        .to_string(),
            },
            None,
        ),
        req @ (EnclaveRequest::AuthorizeRemoteSessionV2 { .. }
        | EnclaveRequest::AuthorizeRemoteSessionV1 { .. }
        | EnclaveRequest::RetireRemoteSessionsV1 { .. }) => {
            (super::remote_sessions::dispatch(req, resident), None)
        }
        req @ (EnclaveRequest::Health | EnclaveRequest::GetPublicKeys) => {
            (super::queries::dispatch(req, resident), None)
        }
        EnclaveRequest::IngestGramineDirectDevOnboardingArtifactV1 {
            artifact,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
        } => (
            complete_gramine_direct_dev_onboarding_ingest_response(
                OnboardingArtifactExpectations {
                    artifact: &artifact,
                    expected_intent_hash,
                    expected_tribute_offer_public,
                    expected_key_epoch,
                    expected_tribute_offer_epoch,
                },
                keys,
                offer_key,
                context.boot,
                context.initialization,
            ),
            None,
        ),
        req @ (EnclaveRequest::GenerateDcapQuote { .. }
        | EnclaveRequest::GenerateTransitionEvidenceDevV1 { .. }
        | EnclaveRequest::SignRegistrationIntentDevV1 { .. }) => {
            (super::registration::dispatch(req, resident), None)
        }
        EnclaveRequest::BeginUpgradeKeyTransferV1 { .. }
        | EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
        | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
        | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
        | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. } => (
            EnclaveResponse::Error {
                message: "onboarding artifact ingest requires an authenticated production session"
                    .into(),
            },
            None,
        ),
        req @ (EnclaveRequest::DkgParticipantAnnounceV1 { .. }
        | EnclaveRequest::DkgOpen { .. }
        | EnclaveRequest::DkgStartDealer { .. }
        | EnclaveRequest::DkgPlayerIngest { .. }
        | EnclaveRequest::DkgDealerReceiveAck { .. }
        | EnclaveRequest::DkgDealerFinalize { .. }
        | EnclaveRequest::DkgPlayerFinalize { .. }
        | EnclaveRequest::DkgTributeOfferPartial { .. }
        | EnclaveRequest::DkgFinalizeTributeOffer { .. }) => {
            (super::dkg_requests::dispatch(req, resident, dkg), None)
        }
    }
}

fn dispatch_payload(
    lane: PayloadLane,
    payload: &[u8],
    resident: RequestContext<'_>,
) -> (EnclaveResponse, Option<&'static str>) {
    let domain_req = match DomainRequest::decode_wire_canonical(payload) {
        Ok(req) => req,
        Err(err) => {
            return (
                EnclaveResponse::Error {
                    message: err.to_string(),
                },
                None,
            );
        }
    };
    let label = domain_req.label();
    if !domain_req.allows_lane(lane) {
        return (
            EnclaveResponse::Error {
                message: format!("domain request {label} is forbidden on lane {lane:?}"),
            },
            Some(label),
        );
    }
    let domain_resp = dispatch_domain_with_context(domain_req, resident);
    let wire_resp = domain_resp
        .into_wire_response()
        .unwrap_or_else(|err| EnclaveResponse::Error {
            message: err.to_string(),
        });
    (wire_resp, Some(label))
}

fn dispatch_domain_with_context(
    req: DomainRequest,
    resident: RequestContext<'_>,
) -> DomainResponse {
    match req {
        req @ (DomainRequest::ExecuteLysis { .. }
        | DomainRequest::MineEncryptedNodV1 { .. }
        | DomainRequest::ReadNodAmountV1 { .. }
        | DomainRequest::EncryptNodForTributeV1 { .. }
        | DomainRequest::IssueNodFromTributeV1 { .. }
        | DomainRequest::ApplyNodGroupLoadOpV1 { .. }
        | DomainRequest::ReadNodGroupLoadV1 { .. }
        | DomainRequest::EncryptNodQuoteV1 { .. }
        | DomainRequest::NodTransferChunkV1 { .. }
        | DomainRequest::ExecuteNodTransferV1 { .. }
        | DomainRequest::ReadNodTransferV1 { .. }
        | DomainRequest::DiscardNodTransferV1 { .. }) => {
            super::nod_requests::dispatch(req, resident)
        }
        req @ (DomainRequest::ApplyTributeDayOpV2 { .. }
        | DomainRequest::ReadTributeDayAmountV2 { .. }
        | DomainRequest::ApplyTributeLeagueOpV1 { .. }
        | DomainRequest::ReadTributeLeagueAmountV1 { .. }
        | DomainRequest::ExecuteLysisFractionsV1 { .. }
        | DomainRequest::ReadTributeDayFractionsV1 { .. }
        | DomainRequest::ProcessEncryptedTributeOfferBatchV2 { .. }
        | DomainRequest::ReadTributeAmountsV2 { .. }
        | DomainRequest::ProcessTributeOfferBatch { .. }) => {
            super::tribute_requests::dispatch(req, resident)
        }
        req @ (DomainRequest::ApplyGratisOp { .. }
        | DomainRequest::ApplyFidelityCohortOp { .. }
        | DomainRequest::SnapshotFidelityLeagues { .. }
        | DomainRequest::QueryFidelityIndex { .. }
        | DomainRequest::ApplyPromisOp { .. }
        | DomainRequest::MineGratisFromPromisV1 { .. }
        | DomainRequest::DeriveAccountKeys { .. }) => super::ledgers::dispatch(req, resident),
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

pub(in crate::transport) fn into_domain_response(
    result: crate::errors::Result<DomainResponse>,
) -> DomainResponse {
    result.unwrap_or_else(|e| DomainResponse::Error {
        message: e.to_string(),
    })
}
