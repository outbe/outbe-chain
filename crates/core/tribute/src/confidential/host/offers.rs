//! Tag 0 session offer batch and tag 11 encrypted offer batch.

use alloy_primitives::B256;
use outbe_core_common::confidential_tee::{verify_response_attestation, OFFER_HKDF_SALT};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::TransportError;

use super::super::encrypted_tribute_offer::process_encrypted_tribute_offer_batch;
use super::super::process::{process_tribute_offer_batch, TributeOfferKeyMaterial};
use super::super::protocol::{
    inputs_canonical_hash, tribute_offer_attestation_preimage, EncryptedTributeOffer,
    TributeOfferResult,
};
use super::super::tribute_v2::{self, EncryptedTributeOfferResultV2};
use super::call::{self, auth_spec};
use super::schema::{EncryptedTributeOfferBatchV2, TributeOfferBatch};

/// Process a cleartext-priced offer batch. Tag 0 is session-only.
pub fn process_offer_batch(
    offers: Vec<EncryptedTributeOffer>,
) -> Result<Vec<TributeOfferResult>, TransportError> {
    let expected = inputs_canonical_hash(&offers);
    let answer = call::session("TributeOfferBatch", move |context| {
        execute_offers(context, &offers, expected)
    })?;
    Ok(answer.results)
}

/// Process one captured resident batch and return the signed enclave hash.
///
/// A hash that differs from [`inputs_canonical_hash`] stays in the batch.
/// [`process_offer_batch`] still rejects that mismatch before authentication.
pub fn process_offer_batch_with(
    context: &InProcessTeeContext,
    offers: Vec<EncryptedTributeOffer>,
) -> Result<TributeOfferBatch, TransportError> {
    let (results, hashed) = call::with_offer(context, |handle| {
        Ok(process_tribute_offer_batch(&material(handle), &offers))
    })?;
    let preimage = tribute_offer_attestation_preimage(hashed, &results).map_err(call::reject)?;
    Ok(TributeOfferBatch {
        results,
        inputs_canonical_hash: hashed,
        attestation_tag: call::sign(context, &preimage),
    })
}

/// Process an encrypted offer batch and record tag 11.
///
/// `chain_id` is the request chain mixed into the frozen inputs hash. The
/// batch itself receives [`call::chain_u64`] so one offer's proof-chain
/// mismatch stays `Rejected` instead of aborting the batch.
pub fn process_encrypted_offer_batch(
    chain_id: u64,
    offers: Vec<EncryptedTributeOffer>,
) -> Result<Vec<EncryptedTributeOfferResultV2>, TransportError> {
    let expected = tribute_v2::encrypted_offer_inputs_hash(chain_id, &offers);
    let answer = call::recorded(
        "process_encrypted_tribute_offer_batch_v2",
        move |context| execute_encrypted(context, chain_id, &offers, expected),
        move |key, value| authenticate_encrypted(key, expected, chain_id, value),
    )?;
    Ok(answer.results)
}

fn execute_offers(
    context: &InProcessTeeContext,
    offers: &[EncryptedTributeOffer],
    expected: B256,
) -> Result<TributeOfferBatch, TransportError> {
    let (results, hashed) = call::with_offer(context, |handle| {
        Ok(process_tribute_offer_batch(&material(handle), offers))
    })?;
    if hashed != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    let preimage = tribute_offer_attestation_preimage(expected, &results).map_err(call::reject)?;
    let answer = TributeOfferBatch {
        results,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate_offers(expected, &context.attestation_public(), &answer)?;
    Ok(answer)
}

fn execute_encrypted(
    context: &InProcessTeeContext,
    chain_id: u64,
    offers: &[EncryptedTributeOffer],
    expected: B256,
) -> Result<EncryptedTributeOfferBatchV2, TransportError> {
    let resident = call::chain_u64(context)?;
    let key = call::tribute_state_key(context)?;
    let (results, hashed) = call::with_offer(context, |handle| {
        Ok(process_encrypted_tribute_offer_batch(
            &material(handle),
            &key,
            resident,
            offers,
        ))
    })?;
    if hashed != expected {
        return Err(call::reject(super::call::HostError::ChainMismatch));
    }
    let preimage = tribute_v2::encrypted_offer_attestation_preimage(expected, &results)
        .map_err(call::reject)?;
    let answer = EncryptedTributeOfferBatchV2 {
        results,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate_encrypted(&context.attestation_public(), expected, chain_id, &answer)?;
    Ok(answer)
}

fn authenticate_offers(
    expected: B256,
    key: &[u8; 32],
    value: &TributeOfferBatch,
) -> Result<(), TransportError> {
    let preimage =
        tribute_offer_attestation_preimage(expected, &value.results).map_err(call::reject)?;
    verify_response_attestation(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        &preimage,
    )
}

fn authenticate_encrypted(
    key: &[u8; 32],
    expected: B256,
    chain_id: u64,
    value: &EncryptedTributeOfferBatchV2,
) -> Result<(), TransportError> {
    let preimage = tribute_v2::encrypted_offer_attestation_preimage(expected, &value.results)
        .map_err(call::reject)?;
    verify_response_attestation(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        &preimage,
    )?;
    call::same_identity(value.results.iter().all(|result| match &result.tribute {
        Some(tribute) => tribute.context.chain_id == chain_id,
        None => true,
    }))
}

fn material(handle: &outbe_tee::resident::OfferKeyHandle) -> TributeOfferKeyMaterial<'_> {
    TributeOfferKeyMaterial {
        tribute_offer_private_key: handle,
        salt: &OFFER_HKDF_SALT,
    }
}
