use alloy_primitives::B256;
use outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2;
use outbe_tee::{
    protocol::EnclaveResponse,
    tribute_day::{self, TributeDayOpRequestV2},
};

use super::tribute::{resident_chain_id, response};
use crate::{
    errors::{EnclaveResult, TransportDispatchError},
    keys::EnclaveKeys,
    transport::SharedTributeOfferKey,
};

pub(super) fn apply(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    request: &TributeDayOpRequestV2,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let derived = offer_key
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if request.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::TributeDayChainDiffersFromResident.into());
        }
        let tribute_state_key =
            crate::tribute_encryption::derive_tribute_state_key(derived.group_sig(), chain, 0)
                .map_err(|_| TransportDispatchError::TributeKeyUnavailable)?;
        let record =
            crate::tribute_day::apply_day_operation(derived.secret(), &tribute_state_key, request)
                .map_err(|_| TransportDispatchError::TributeDayOperationFailed)?;
        let inputs_canonical_hash = tribute_day::day_operation_inputs_hash(request)
            .map_err(|_| TransportDispatchError::CannotEncodeTributeDayInputs)?;
        let preimage =
            tribute_day::day_operation_attestation_preimage(inputs_canonical_hash, &record)
                .map_err(|_| TransportDispatchError::CannotEncodeTributeDayResult)?;
        Ok(EnclaveResponse::TributeDayOpAppliedV2 {
            record,
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) fn read(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    record: &EncryptedTributeDayAmountV2,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let derived = offer_key
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if record.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::TributeDayChainDiffersFromResident.into());
        }
        let amount = crate::tribute_day::read_day_amount(derived.secret(), record)
            .map_err(|_| TransportDispatchError::TributeDayDecryptionFailed)?;
        let inputs_canonical_hash = tribute_day::day_read_inputs_hash(record)
            .map_err(|_| TransportDispatchError::CannotEncodeTributeDayReadInputs)?;
        let preimage = tribute_day::day_read_attestation_preimage(inputs_canonical_hash, amount);
        Ok(EnclaveResponse::TributeDayAmountReadV2 {
            amount,
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}
