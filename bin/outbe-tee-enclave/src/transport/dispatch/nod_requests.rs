//! Route requests within one domain family.

use super::requests::RequestContext;
use crate::transport::*;

pub(super) fn dispatch(req: EnclaveRequest, context: RequestContext<'_>) -> EnclaveResponse {
    let RequestContext {
        keys,
        offer_key,
        chain_id,
        ..
    } = context;
    match req {
        #[cfg(not(feature = "e2e-test"))]
        EnclaveRequest::CreateNodForTestV2 { .. } => EnclaveResponse::Error {
            message: "NOD fixture command disabled".into(),
        },
        #[cfg(feature = "e2e-test")]
        EnclaveRequest::CreateNodForTestV2 {
            terms,
            creator_public,
            amount,
        } => super::nod::create_for_test(
            keys,
            offer_key,
            chain_id,
            super::nod::NodFixtureInput {
                terms,
                creator_public,
                amount,
            },
        ),
        EnclaveRequest::PrepareEncryptedNodsV2 { request } => {
            super::nod::prepare(keys, offer_key, chain_id, &request)
        }
        EnclaveRequest::OpenEncryptedNodsV2 { authority, carrier } => {
            super::nod::open(keys, offer_key, chain_id, &authority, &carrier)
        }
        EnclaveRequest::MineEncryptedNodV2 { request } => {
            super::nod::mine(keys, offer_key, chain_id, &request)
        }
        EnclaveRequest::ReadNodAmountV2 { nod } => {
            super::nod::read_amount(keys, offer_key, chain_id, &nod)
        }
        EnclaveRequest::NodTransferChunkV2 {
            id,
            total,
            offset,
            bytes,
        } => super::nod_transfer::append(id, total, offset, &bytes),
        EnclaveRequest::ExecuteNodTransferV2 { id } => {
            super::nod_transfer::execute(id, keys, offer_key, chain_id)
        }
        EnclaveRequest::ReadNodTransferV2 { id, offset } => super::nod_transfer::read(id, offset),
        EnclaveRequest::DiscardNodTransferV2 { id } => super::nod_transfer::discard(id),
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}
