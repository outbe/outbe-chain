//! Tribute offers, private reads and deterministic day operations.
use super::*;
use crate::{protocol::*, tribute_day, tribute_v2};
use outbe_core_common::tribute_encryption::EncryptedTributeV2;

pub(super) fn offers(
    context: &InProcessTeeContext,
    offers: &[EncryptedTributeOffer],
) -> Result<EnclaveResponse> {
    let (results, inputs_canonical_hash) = context.with_offer_key_handle(|handle| {
        crate::process::process_tribute_offer_batch(
            &crate::process::TributeOfferKeyMaterial {
                tribute_offer_private_key: handle,
                salt: &crate::OFFER_HKDF_SALT,
            },
            offers,
        )
    })?;
    let attestation_tag = sign(
        context,
        &tribute_offer_attestation_preimage(inputs_canonical_hash, &results)?,
    );
    Ok(EnclaveResponse::TributeOfferBatch {
        results,
        inputs_canonical_hash,
        attestation_tag,
    })
}
pub(super) fn encrypted_offers(
    context: &InProcessTeeContext,
    offers: &[EncryptedTributeOffer],
) -> Result<EnclaveResponse> {
    let key = state(context, crate::tribute_encryption::derive_tribute_state_key)?;
    let chain_id = chain_u64(context)?;
    let (results, inputs_canonical_hash) = context.with_offer_key_handle(|handle| {
        crate::encrypted_tribute_offer::process_encrypted_tribute_offer_batch(
            &crate::process::TributeOfferKeyMaterial {
                tribute_offer_private_key: handle,
                salt: &crate::OFFER_HKDF_SALT,
            },
            &key,
            chain_id,
            offers,
        )
    })?;
    let attestation_tag = sign(
        context,
        &tribute_v2::encrypted_offer_attestation_preimage(inputs_canonical_hash, &results)?,
    );
    Ok(EnclaveResponse::EncryptedTributeOfferBatchV2 {
        results,
        inputs_canonical_hash,
        attestation_tag,
    })
}
pub(super) fn read(
    context: &InProcessTeeContext,
    tributes: &[EncryptedTributeV2],
) -> Result<EnclaveResponse> {
    let key = state(context, crate::tribute_encryption::derive_tribute_state_key)?;
    let amounts = tributes
        .iter()
        .map(|tribute| {
            require_chain(context, tribute.context.chain_id)?;
            Ok(crate::tribute_encryption::decrypt_tribute(&key, tribute)?)
        })
        .collect::<Result<Vec<_>>>()?;
    let inputs_canonical_hash = tribute_v2::tribute_read_inputs_hash(tributes)?;
    let attestation_tag = sign(
        context,
        &tribute_v2::tribute_read_attestation_preimage(inputs_canonical_hash, &amounts)?,
    );
    Ok(EnclaveResponse::TributeAmountsReadV2 {
        amounts,
        inputs_canonical_hash,
        attestation_tag,
    })
}
pub(super) fn day(
    context: &InProcessTeeContext,
    request: EnclaveRequest,
) -> Result<EnclaveResponse> {
    context.with_offer_key_handle(|offer| -> Result<EnclaveResponse> {
        let secret = offer.key_handle();
        match request {
            EnclaveRequest::ApplyTributeDayOpV2 { request } => {
                require_chain(context, request.chain_id)?;
                let tribute = state(context, crate::tribute_encryption::derive_tribute_state_key)?;
                let record = tribute_day::apply_day_operation(secret, &tribute, &request)?;
                let inputs_canonical_hash = tribute_day::day_operation_inputs_hash(&request)?;
                let attestation_tag = sign(
                    context,
                    &tribute_day::day_operation_attestation_preimage(
                        inputs_canonical_hash,
                        &record,
                    )?,
                );
                Ok(EnclaveResponse::TributeDayOpAppliedV2 {
                    record,
                    inputs_canonical_hash,
                    attestation_tag,
                })
            }
            EnclaveRequest::ReadTributeDayAmountV2 { record } => {
                require_chain(context, record.chain_id)?;
                let amount = tribute_day::read_day_amount(secret, &record)?;
                let inputs_canonical_hash = tribute_day::day_read_inputs_hash(&record)?;
                let attestation_tag = sign(
                    context,
                    &tribute_day::day_read_attestation_preimage(inputs_canonical_hash, amount),
                );
                Ok(EnclaveResponse::TributeDayAmountReadV2 {
                    amount,
                    inputs_canonical_hash,
                    attestation_tag,
                })
            }
            EnclaveRequest::ExecuteLysisFractionsV1 { request } => {
                require_chain(context, request.chain_id)?;
                let tribute = state(context, crate::tribute_encryption::derive_tribute_state_key)?;
                let nod = state(context, crate::nod_encryption::derive_nod_state_key)?;
                let result =
                    tribute_day::execute_lysis_fractions(secret, &tribute, &nod, &request)?;
                let inputs_canonical_hash = tribute_day::lysis_fractions_inputs_hash(&request)?;
                let attestation_tag = sign(
                    context,
                    &tribute_day::lysis_fractions_attestation_preimage(
                        inputs_canonical_hash,
                        &result,
                    )?,
                );
                Ok(EnclaveResponse::LysisFractionsExecutedV1 {
                    result: Box::new(result),
                    inputs_canonical_hash,
                    attestation_tag,
                })
            }
            EnclaveRequest::ReadTributeDayFractionsV1 { record } => {
                require_chain(context, record.chain_id)?;
                let fractions = tribute_day::read_day_fractions(secret, &record)?;
                let inputs_canonical_hash = tribute_day::day_fractions_read_inputs_hash(&record)?;
                let attestation_tag = sign(
                    context,
                    &tribute_day::day_fractions_read_attestation_preimage(
                        inputs_canonical_hash,
                        &fractions,
                    )?,
                );
                Ok(EnclaveResponse::TributeDayFractionsReadV1 {
                    fractions,
                    inputs_canonical_hash,
                    attestation_tag,
                })
            }
            _ => unreachable!("exhaustive parent routes only day operations"),
        }
    })?
}
