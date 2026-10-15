//! Installed-network-key-only Tribute commands.

use alloy_primitives::{B256, U256};
use outbe_primitives::tribute_encryption::EncryptedTributeV2;
use outbe_tee::{
    protocol::{EnclaveResponse, EncryptedTributeOffer},
    tribute_v2,
};

use crate::{
    errors::{EnclaveResult, TransportDispatchError},
    keys::EnclaveKeys,
    transport::SharedTributeOfferKey,
};

pub(super) fn process_offers(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    offers: &[EncryptedTributeOffer],
) -> EnclaveResponse {
    let result = (|| -> EnclaveResult<EnclaveResponse> {
        let derived = offer_key
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        let chain_id = resident_chain_id(chain)?;
        let key = keys.tribute_offer_key_material_with(derived.secret());
        let (results, inputs_canonical_hash) =
            crate::encrypted_tribute_offer::process_encrypted_tribute_offer_batch(
                &key, chain_id, offers,
            )
            .map_err(|_| TransportDispatchError::TributeCryptoBackendUnavailable)?;
        let preimage =
            tribute_v2::encrypted_offer_attestation_preimage(inputs_canonical_hash, &results)
                .map_err(|_| TransportDispatchError::CannotEncodeEncryptedTributeResults)?;
        Ok(EnclaveResponse::EncryptedTributeOfferBatchV2 {
            results,
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })();
    response(result)
}

pub(super) fn read_amounts(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    tributes: &[EncryptedTributeV2],
) -> EnclaveResponse {
    let result = (|| -> EnclaveResult<EnclaveResponse> {
        let derived = offer_key
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        let chain_id = resident_chain_id(chain)?;
        let amounts = tributes
            .iter()
            .map(|tribute| -> EnclaveResult<_> {
                if tribute.context.chain_id != chain_id {
                    return Err(TransportDispatchError::TributeChainDiffersFromResident.into());
                }
                crate::tribute_encryption::decrypt_tribute(derived.secret(), tribute)
                    .map_err(|_| TransportDispatchError::TributeDecryptionFailed.into())
            })
            .collect::<EnclaveResult<Vec<_>>>()?;
        let inputs_canonical_hash = tribute_v2::tribute_read_inputs_hash(tributes)
            .map_err(|_| TransportDispatchError::CannotEncodeTributeReadInputs)?;
        let preimage =
            tribute_v2::tribute_read_attestation_preimage(inputs_canonical_hash, &amounts)
                .map_err(|_| TransportDispatchError::CannotEncodeTributeReadResults)?;
        Ok(EnclaveResponse::TributeAmountsReadV2 {
            amounts,
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })();
    response(result)
}

pub(super) fn resident_chain_id(chain: B256) -> EnclaveResult<u64> {
    let chain = U256::from_be_bytes(chain.0);
    if chain > U256::from(u64::MAX) {
        return Err(TransportDispatchError::ResidentChainIdExceedsU64.into());
    }
    Ok(chain.to())
}

pub(super) fn response(result: EnclaveResult<EnclaveResponse>) -> EnclaveResponse {
    result.unwrap_or_else(|message| EnclaveResponse::Error {
        message: message.to_string(),
    })
}
