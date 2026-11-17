//! NOD transitions and signed deterministic refusals.
use super::*;
use crate::{nod_mine, response_auth};

pub(super) fn dispatch(
    context: &InProcessTeeContext,
    request: EnclaveRequest,
) -> Result<EnclaveResponse> {
    let nod_key = state(context, crate::nod_encryption::derive_nod_state_key)?;
    match request {
        EnclaveRequest::MineEncryptedNodV1 { request } => {
            require_chain(context, request.nod.terms.chain_id)?;
            let gratis = state(context, crate::gratis::derive_gratis_state_key)?;
            let fidelity = state(context, crate::fidelity::derive_fidelity_state_key)?;
            let mut result = match nod_mine::apply(&nod_key, &gratis, &fidelity, &request) {
                Ok(result) => result,
                Err(crate::errors::TeeError::TributeOfferReject(reason)) => {
                    let inputs_canonical_hash = nod_mine::inputs_hash(&request)?;
                    let attestation_tag = sign(
                        context,
                        &response_auth::attestation(
                            b"outbe/nod/mint-rejected/v1",
                            inputs_canonical_hash,
                            &reason,
                        )?,
                    );
                    return Ok(EnclaveResponse::EncryptedNodMintRejectedV1 {
                        reason,
                        inputs_canonical_hash,
                        attestation_tag,
                    });
                }
                Err(error) => return Err(error.into()),
            };
            result.attestation_tag = sign(context, &nod_mine::result_preimage(&result)?);
            Ok(EnclaveResponse::EncryptedNodMinedV1 {
                result: Box::new(result),
            })
        }
        EnclaveRequest::ReadNodAmountV1 { nod } => {
            require_chain(context, nod.terms.chain_id)?;
            let amount = crate::nod_encryption::decrypt_nod(&nod_key, &nod)?;
            let inputs_canonical_hash = nod_mine::nod_read_hash(&nod)?;
            let attestation_tag = sign(
                context,
                &nod_mine::nod_read_preimage(inputs_canonical_hash, amount),
            );
            Ok(EnclaveResponse::NodAmountReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            })
        }
        EnclaveRequest::EncryptNodForTributeV1 {
            tribute,
            terms,
            amount,
        } => {
            require_chain(context, terms.chain_id)?;
            let tribute_key = state(context, crate::tribute_encryption::derive_tribute_state_key)?;
            let inputs_canonical_hash = response_auth::hash(
                b"outbe/nod/tribute-create-inputs/v1",
                &(&tribute, &terms, amount),
            )?;
            let nod = crate::nod_encryption::encrypt_nod_for_tribute(
                &tribute_key,
                &nod_key,
                &tribute,
                terms,
                amount,
            )?;
            let attestation_tag = sign(
                context,
                &response_auth::attestation(
                    b"outbe/nod/tribute-create-result/v1",
                    inputs_canonical_hash,
                    &nod,
                )?,
            );
            Ok(EnclaveResponse::NodEncryptedForTributeV1 {
                nod,
                inputs_canonical_hash,
                attestation_tag,
            })
        }
        EnclaveRequest::IssueNodFromTributeV1 { request } => {
            require_chain(context, request.terms.chain_id)?;
            let tribute = state(context, crate::tribute_encryption::derive_tribute_state_key)?;
            let inputs_canonical_hash =
                response_auth::hash(b"outbe/nod/issue-from-tribute-inputs/v1", request.as_ref())?;
            let outcome = context.with_offer_key_handle(|offer| {
                crate::nod_encryption::issue_nod_from_tribute(
                    offer.key_handle(),
                    &tribute,
                    &nod_key,
                    &request,
                )
            })??;
            let attestation_tag = sign(
                context,
                &response_auth::attestation(
                    b"outbe/nod/issue-from-tribute-result/v1",
                    inputs_canonical_hash,
                    &outcome,
                )?,
            );
            Ok(EnclaveResponse::NodIssuedFromTributeV1 {
                outcome,
                inputs_canonical_hash,
                attestation_tag,
            })
        }
        EnclaveRequest::ApplyNodGroupLoadOpV1 { request } => {
            require_chain(context, request.chain_id)?;
            let inputs_canonical_hash = nod_mine::nod_group_load_inputs_hash(&request)?;
            let record = crate::nod_encryption::apply_nod_group_load_operation(&nod_key, &request)?;
            let attestation_tag = sign(
                context,
                &nod_mine::nod_group_load_attestation_preimage(inputs_canonical_hash, &record)?,
            );
            Ok(EnclaveResponse::NodGroupLoadOpAppliedV1 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            })
        }
        EnclaveRequest::ReadNodGroupLoadV1 { record } => {
            require_chain(context, record.chain_id)?;
            let amount = crate::nod_encryption::read_nod_group_load(&nod_key, &record)?;
            let inputs_canonical_hash = nod_mine::nod_group_load_read_hash(&record)?;
            let attestation_tag = sign(
                context,
                &nod_mine::nod_group_load_read_preimage(inputs_canonical_hash, amount),
            );
            Ok(EnclaveResponse::NodGroupLoadReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            })
        }
        EnclaveRequest::EncryptNodQuoteV1 {
            nod,
            context: quote,
            payment_minor,
        } => {
            require_chain(context, quote.chain_id)?;
            let inputs_canonical_hash = response_auth::hash(
                b"outbe/nod/quote-encrypt-inputs/v1",
                &(nod.as_ref(), &quote, payment_minor),
            )?;
            let encrypted_payment =
                crate::nod_encryption::encrypt_nod_quote(&nod_key, &nod, &quote, payment_minor)?;
            let attestation_tag = sign(
                context,
                &response_auth::attestation(
                    b"outbe/nod/quote-encrypt-result/v1",
                    inputs_canonical_hash,
                    &encrypted_payment,
                )?,
            );
            Ok(EnclaveResponse::NodQuoteEncryptedV1 {
                encrypted_payment,
                inputs_canonical_hash,
                attestation_tag,
            })
        }
        _ => unreachable!("exhaustive parent routes only NOD operations"),
    }
}
