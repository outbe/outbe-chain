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
        EnclaveRequest::ApplyTributeDayOpV2 { request } => {
            super::tribute_day::apply(keys, offer_key, chain_id, &request)
        }
        EnclaveRequest::ReadTributeDayAmountV2 { record } => {
            super::tribute_day::read(keys, offer_key, chain_id, &record)
        }
        EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { offers } => {
            super::tribute::process_offers(keys, offer_key, chain_id, &offers)
        }
        EnclaveRequest::ReadTributeAmountsV2 { tributes } => {
            super::tribute::read_amounts(keys, offer_key, chain_id, &tributes)
        }
        EnclaveRequest::ProcessTributeOfferBatch { offers } => {
            let derived = offer_key.get();
            let km = match derived {
                Some(d) => keys.tribute_offer_key_material_with(d.secret()),
                None => keys.tribute_offer_key_material(),
            };
            let (results, inputs_canonical_hash) = process_tribute_offer_batch(&km, &offers);
            // Sign (inputs_canonical_hash || results) with the enclave's
            // Ed25519 attestation key. The host verifies this signature against the
            // attestation key it pinned from the quote. This proves that this attested
            // enclave produced the results and that the host did not substitute them.
            let preimage = outbe_tee::protocol::tribute_offer_attestation_preimage(
                inputs_canonical_hash,
                &results,
            );
            let attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::TributeOfferBatch {
                results,
                inputs_canonical_hash,
                attestation_tag,
            }
        }
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}
