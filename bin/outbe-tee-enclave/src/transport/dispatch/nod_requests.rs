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
        EnclaveRequest::ExecuteLysis { request } => {
            super::nod::execute_lysis(keys, offer_key, chain_id, &request)
        }
        #[cfg(not(feature = "e2e-test"))]
        EnclaveRequest::CreateNodForTestV1 { .. } => EnclaveResponse::Error {
            message: "NOD fixture command disabled".into(),
        },
        #[cfg(feature = "e2e-test")]
        EnclaveRequest::CreateNodForTestV1 {
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
        EnclaveRequest::MineEncryptedNodV1 { request } => {
            super::nod::mine(keys, offer_key, chain_id, &request)
        }
        EnclaveRequest::ReadNodAmountV1 { nod } => {
            super::nod::read_amount(keys, offer_key, chain_id, &nod)
        }
        EnclaveRequest::EncryptNodForTributeV1 {
            tribute,
            terms,
            amount,
        } => super::nod::encrypt_for_tribute(
            keys,
            offer_key,
            chain_id,
            super::nod::NodTributeInput {
                tribute: &tribute,
                terms,
                amount,
            },
        ),
        EnclaveRequest::NodTransferChunkV1 {
            id,
            total,
            offset,
            bytes,
        } => super::nod_transfer::append(id, total, offset, &bytes),
        EnclaveRequest::ExecuteNodTransferV1 { id } => {
            super::nod_transfer::execute(id, keys, offer_key, chain_id)
        }
        EnclaveRequest::ReadNodTransferV1 { id, offset } => super::nod_transfer::read(id, offset),
        EnclaveRequest::DiscardNodTransferV1 { id } => super::nod_transfer::discard(id),
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}
