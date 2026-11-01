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
        EnclaveRequest::IssueNodFromTributeV1 { request } => {
            super::nod::issue_from_tribute(keys, offer_key, chain_id, &request)
        }
        EnclaveRequest::ApplyNodGroupLoadOpV1 { request } => {
            super::nod::apply_group_load_op(keys, offer_key, chain_id, &request)
        }
        EnclaveRequest::ReadNodGroupLoadV1 { record } => {
            super::nod::read_group_load(keys, offer_key, chain_id, &record)
        }
        EnclaveRequest::EncryptNodQuoteV1 {
            nod,
            context,
            payment_minor,
        } => super::nod::encrypt_quote(keys, offer_key, chain_id, (&nod, &context, payment_minor)),
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
