use alloy_primitives::B256;
use outbe_core_common::tribute_day_encryption::{
    EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1,
};
use outbe_enclave::{
    protocol::EnclaveResponse,
    tribute_day::{self, LysisFractionsRequestV1, TributeDayOpRequestV2},
};

use super::{
    requests::DomainDispatchResult,
    tribute::{resident_chain_id, response},
};
use crate::{
    errors::{EnclaveResult, TransportDispatchError},
    keys::EnclaveKeys,
    transport::{DerivedTributeOfferKey, SharedTributeOfferKey},
};

fn require_resident_key(
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    request_chain_id: u64,
) -> EnclaveResult<&DerivedTributeOfferKey> {
    let derived = offer_key
        .get()
        .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
    if request_chain_id != resident_chain_id(chain)? {
        return Err(TransportDispatchError::TributeDayChainDiffersFromResident.into());
    }
    Ok(derived)
}

fn derive_tribute_key(
    derived: &DerivedTributeOfferKey,
    chain: B256,
) -> EnclaveResult<zeroize::Zeroizing<[u8; 32]>> {
    crate::tribute_encryption::derive_tribute_state_key(derived.group_sig(), chain, 0)
        .map_err(|_| TransportDispatchError::TributeKeyUnavailable.into())
}

pub(super) fn apply(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    request: &TributeDayOpRequestV2,
) -> DomainDispatchResult {
    dispatch_tribute_op(
        (keys, offer_key, chain, request.chain_id),
        |secret, tribute_key| crate::tribute_day::apply_day_operation(secret, tribute_key, request),
        || tribute_day::day_operation_inputs_hash(request),
        tribute_day::day_operation_attestation_preimage,
        |record, inputs_canonical_hash, attestation_tag| EnclaveResponse::TributeDayOpAppliedV2 {
            record,
            inputs_canonical_hash,
            attestation_tag,
        },
    )
}

pub(super) fn read(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    record: &EncryptedTributeDayAmountV2,
) -> DomainDispatchResult {
    dispatch_amount_read(
        (keys, offer_key, chain, record.chain_id),
        |secret| crate::tribute_day::read_day_amount(secret, record),
        || tribute_day::day_read_inputs_hash(record),
        tribute_day::day_read_attestation_preimage,
        |amount, inputs_canonical_hash, attestation_tag| EnclaveResponse::TributeDayAmountReadV2 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        },
    )
}

fn dispatch_tribute_op<R>(
    (keys, offer_key, chain, request_chain_id): (&EnclaveKeys, &SharedTributeOfferKey, B256, u64),
    op_fn: impl FnOnce(&[u8; 32], &[u8; 32]) -> crate::errors::Result<R>,
    hash_fn: impl FnOnce() -> Result<B256, serde_json::Error>,
    preimage_fn: fn(B256, &R) -> Result<Vec<u8>, serde_json::Error>,
    wrap_fn: fn(R, B256, Vec<u8>) -> EnclaveResponse,
) -> DomainDispatchResult {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let derived = require_resident_key(offer_key, chain, request_chain_id)?;
        let tribute_state_key = derive_tribute_key(derived, chain)?;
        let record = op_fn(derived.secret(), &tribute_state_key)
            .map_err(|_| TransportDispatchError::TributeDayOperationFailed)?;
        let inputs_canonical_hash =
            hash_fn().map_err(|_| TransportDispatchError::CannotEncodeTributeDayInputs)?;
        let preimage = preimage_fn(inputs_canonical_hash, &record)
            .map_err(|_| TransportDispatchError::CannotEncodeTributeDayResult)?;
        Ok(wrap_fn(
            record,
            inputs_canonical_hash,
            keys.sign_attestation(&preimage).to_vec(),
        ))
    })())
}

fn dispatch_amount_read(
    (keys, offer_key, chain, record_chain_id): (&EnclaveKeys, &SharedTributeOfferKey, B256, u64),
    read_fn: impl FnOnce(&[u8; 32]) -> crate::errors::Result<alloy_primitives::U256>,
    hash_fn: impl FnOnce() -> Result<B256, serde_json::Error>,
    preimage_fn: fn(B256, alloy_primitives::U256) -> Vec<u8>,
    wrap_fn: fn(alloy_primitives::U256, B256, Vec<u8>) -> EnclaveResponse,
) -> DomainDispatchResult {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let derived = require_resident_key(offer_key, chain, record_chain_id)?;
        let amount = read_fn(derived.secret())
            .map_err(|_| TransportDispatchError::TributeDayDecryptionFailed)?;
        let inputs_canonical_hash =
            hash_fn().map_err(|_| TransportDispatchError::CannotEncodeTributeDayReadInputs)?;
        let preimage = preimage_fn(inputs_canonical_hash, amount);
        Ok(wrap_fn(
            amount,
            inputs_canonical_hash,
            keys.sign_attestation(&preimage).to_vec(),
        ))
    })())
}

pub(super) fn execute_lysis_fractions(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    request: &LysisFractionsRequestV1,
) -> DomainDispatchResult {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let derived = require_resident_key(offer_key, chain, request.chain_id)?;
        let tribute_state_key = derive_tribute_key(derived, chain)?;
        let nod_state_key =
            crate::nod_encryption::derive_nod_state_key(derived.group_sig(), chain, 0)
                .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let result = crate::tribute_day::execute_lysis_fractions(
            derived.secret(),
            &tribute_state_key,
            &nod_state_key,
            request,
        )
        .map_err(|_| TransportDispatchError::TributeDayOperationFailed)?;
        let inputs_canonical_hash = tribute_day::lysis_fractions_inputs_hash(request)
            .map_err(|_| TransportDispatchError::CannotEncodeTributeDayInputs)?;
        let preimage =
            tribute_day::lysis_fractions_attestation_preimage(inputs_canonical_hash, &result)
                .map_err(|_| TransportDispatchError::CannotEncodeTributeDayResult)?;
        Ok(EnclaveResponse::LysisFractionsExecutedV1 {
            result: Box::new(result),
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) fn read_day_fractions(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    record: &EncryptedTributeDayFractionsV1,
) -> DomainDispatchResult {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let derived = require_resident_key(offer_key, chain, record.chain_id)?;
        let fractions = crate::tribute_day::read_day_fractions(derived.secret(), record)
            .map_err(|_| TransportDispatchError::TributeDayDecryptionFailed)?;
        let inputs_canonical_hash = tribute_day::day_fractions_read_inputs_hash(record)
            .map_err(|_| TransportDispatchError::CannotEncodeTributeDayReadInputs)?;
        let preimage =
            tribute_day::day_fractions_read_attestation_preimage(inputs_canonical_hash, &fractions)
                .map_err(|_| TransportDispatchError::CannotEncodeTributeDayResult)?;
        Ok(EnclaveResponse::TributeDayFractionsReadV1 {
            fractions,
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}
