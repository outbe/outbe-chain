//! Direct domain dispatch against RAM-only resident handles.
use super::protocol::{EnclaveRequest, EnclaveResponse};
use alloy_primitives::{B256, U256};
use outbe_tee::{
    in_process::InProcessTeeContext, protocol::PayloadLane, resident::StateKeyHandle,
    InProcessDispatchError, TransportError,
};

mod ledgers;
mod nod;
#[cfg(test)]
pub(crate) mod tests;
mod tribute;

/// Typed domain failures retained through the infrastructure transport boundary.
#[derive(Debug, thiserror::Error)]
pub enum DomainDispatchError {
    #[error("domain engine failed: {0}")]
    Engine(#[from] crate::errors::TeeError),
    #[error("domain computation failed: {0}")]
    Compute(#[from] crate::errors::EnclaveError),
    #[error("resident operation failed: {0}")]
    Resident(#[from] outbe_tee::in_process::InProcessError),
    #[error("canonical attestation encoding failed: {0}")]
    Attestation(#[from] serde_json::Error),
    #[error("request chain differs from resident chain")]
    ChainMismatch,
    #[error("resident chain identifier exceeds u64")]
    ChainOverflow,
    #[error("owner signature must be 65 bytes")]
    OwnerSignatureLength,
}
impl From<DomainDispatchError> for TransportError {
    fn from(error: DomainDispatchError) -> Self {
        match error {
            DomainDispatchError::Resident(error) => InProcessDispatchError::Resident(error).into(),
            error => InProcessDispatchError::Domain(Box::new(error)).into(),
        }
    }
}
type Result<T> = std::result::Result<T, DomainDispatchError>;

/// Install the resident context with this engine's lane-checked domain dispatcher.
pub fn install(
    context: std::sync::Arc<InProcessTeeContext>,
) -> std::result::Result<(), outbe_tee::InstallError> {
    outbe_tee::install_in_process_tee_context(context, dispatch_payload)
}

/// Decode canonical payloads and enforce the lane allowlist before accessing any key.
pub fn dispatch_payload(
    context: &InProcessTeeContext,
    lane: PayloadLane,
    payload: &[u8],
) -> std::result::Result<outbe_tee::protocol::EnclaveResponse, TransportError> {
    let request = EnclaveRequest::decode_wire_canonical(payload)?;
    if !request.allows_lane(lane) {
        return Err(InProcessDispatchError::LaneDenied { lane }.into());
    }
    let response = dispatch(context, request)?;
    response.into_wire_response()
}

fn dispatch(context: &InProcessTeeContext, request: EnclaveRequest) -> Result<EnclaveResponse> {
    match request {
        EnclaveRequest::ApplyGratisOp { request } => ledgers::gratis(context, &request),
        EnclaveRequest::ApplyPromisOp { request } => ledgers::promis(context, &request),
        EnclaveRequest::MineGratisFromPromisV1 { request } => {
            ledgers::mine_gratis(context, &request)
        }
        EnclaveRequest::ApplyFidelityCohortOp { request } => ledgers::fidelity(context, &request),
        EnclaveRequest::SnapshotFidelityLeagues { request } => ledgers::snapshot(context, &request),
        EnclaveRequest::QueryFidelityIndex { request } => ledgers::query(context, &request),
        EnclaveRequest::DeriveAccountKeys {
            ledger,
            account,
            requester_ephemeral_pubkey,
            owner_sig,
        } => ledgers::account_keys(
            context,
            ledger,
            account,
            requester_ephemeral_pubkey,
            &owner_sig,
        ),
        EnclaveRequest::ProcessTributeOfferBatch { offers } => tribute::offers(context, &offers),
        EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { offers } => {
            tribute::encrypted_offers(context, &offers)
        }
        EnclaveRequest::ReadTributeAmountsV2 { tributes } => tribute::read(context, &tributes),
        request @ (EnclaveRequest::ApplyTributeDayOpV2 { .. }
        | EnclaveRequest::ReadTributeDayAmountV2 { .. }
        | EnclaveRequest::ExecuteLysisFractionsV1 { .. }
        | EnclaveRequest::ReadTributeDayFractionsV1 { .. }) => tribute::day(context, request),
        request @ (EnclaveRequest::MineEncryptedNodV1 { .. }
        | EnclaveRequest::ReadNodAmountV1 { .. }
        | EnclaveRequest::EncryptNodForTributeV1 { .. }
        | EnclaveRequest::IssueNodFromTributeV1 { .. }
        | EnclaveRequest::ApplyNodGroupLoadOpV1 { .. }
        | EnclaveRequest::ReadNodGroupLoadV1 { .. }
        | EnclaveRequest::EncryptNodQuoteV1 { .. }) => nod::dispatch(context, request),
    }
}

fn chain(context: &InProcessTeeContext) -> B256 {
    B256::from(context.manifest().chain_id)
}
fn chain_u64(context: &InProcessTeeContext) -> Result<u64> {
    let chain = U256::from_be_bytes(context.manifest().chain_id);
    u64::try_from(chain).map_err(|_| DomainDispatchError::ChainOverflow)
}
fn require_chain(context: &InProcessTeeContext, requested: u64) -> Result<()> {
    if requested != chain_u64(context)? {
        return Err(DomainDispatchError::ChainMismatch);
    }
    Ok(())
}
fn state(
    context: &InProcessTeeContext,
    derive: fn(
        &outbe_tee::resident::ResidentOfferMaterial,
        B256,
        u64,
    ) -> crate::errors::Result<StateKeyHandle>,
) -> Result<StateKeyHandle> {
    Ok(context.with_offer_material(|material| derive(material, chain(context), 0))??)
}
fn sign(context: &InProcessTeeContext, preimage: &[u8]) -> Vec<u8> {
    context.sign_domain_attestation(preimage).to_vec()
}
