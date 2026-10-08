use alloy_primitives::B256;
use outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2;
use outbe_tee::{
    protocol::EnclaveResponse,
    tribute_day::{self, TributeDayOpRequestV2},
};

use super::tribute::{resident_chain_id, response};
use crate::{keys::EnclaveKeys, transport::SharedTributeOfferKey};

pub(super) fn apply(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    request: &TributeDayOpRequestV2,
) -> EnclaveResponse {
    response((|| {
        let derived = offer_key.get().ok_or("no resident network key")?;
        if request.chain_id != resident_chain_id(chain)? {
            return Err("Tribute day chain differs from resident chain");
        }
        let record = crate::tribute_day::apply_day_operation(derived.secret(), request)
            .map_err(|_| "Tribute day operation failed")?;
        let inputs_canonical_hash = tribute_day::day_operation_inputs_hash(request)
            .map_err(|_| "cannot encode Tribute day inputs")?;
        let preimage =
            tribute_day::day_operation_attestation_preimage(inputs_canonical_hash, &record)
                .map_err(|_| "cannot encode Tribute day result")?;
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
    response((|| {
        let derived = offer_key.get().ok_or("no resident network key")?;
        if record.chain_id != resident_chain_id(chain)? {
            return Err("Tribute day chain differs from resident chain");
        }
        let amount = crate::tribute_day::read_day_amount(derived.secret(), record)
            .map_err(|_| "Tribute day decryption failed")?;
        let inputs_canonical_hash = tribute_day::day_read_inputs_hash(record)
            .map_err(|_| "cannot encode Tribute day read inputs")?;
        let preimage = tribute_day::day_read_attestation_preimage(inputs_canonical_hash, amount);
        Ok(EnclaveResponse::TributeDayAmountReadV2 {
            amount,
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}
