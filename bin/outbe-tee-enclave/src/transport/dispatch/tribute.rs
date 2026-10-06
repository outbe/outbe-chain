//! Installed-network-key-only Tribute commands.

use alloy_primitives::{B256, U256};
use outbe_primitives::tribute_encryption::EncryptedTributeV2;
use outbe_tee::{
    protocol::{EnclaveResponse, EncryptedTributeOffer},
    tribute_v2,
};

use crate::{keys::EnclaveKeys, transport::SharedTributeOfferKey};

pub(super) fn process_offers(
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    chain: B256,
    offers: &[EncryptedTributeOffer],
) -> EnclaveResponse {
    let result = (|| {
        let derived = offer_key.get().ok_or("no resident network key")?;
        let chain_id = resident_chain_id(chain)?;
        let key = keys.tribute_offer_key_material_with(derived.secret());
        let (results, inputs_canonical_hash) =
            crate::encrypted_tribute_offer::process_encrypted_tribute_offer_batch(
                &key, chain_id, offers,
            );
        let preimage =
            tribute_v2::encrypted_offer_attestation_preimage(inputs_canonical_hash, &results)
                .map_err(|_| "cannot encode encrypted Tribute results")?;
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
    let result = (|| {
        let derived = offer_key.get().ok_or("no resident network key")?;
        let chain_id = resident_chain_id(chain)?;
        let amounts = tributes
            .iter()
            .map(|tribute| {
                if tribute.context.chain_id != chain_id {
                    return Err("Tribute chain differs from resident chain");
                }
                crate::tribute_encryption::decrypt_tribute(derived.secret(), tribute)
                    .map_err(|_| "Tribute decryption failed")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let inputs_canonical_hash = tribute_v2::tribute_read_inputs_hash(tributes)
            .map_err(|_| "cannot encode Tribute read inputs")?;
        let preimage =
            tribute_v2::tribute_read_attestation_preimage(inputs_canonical_hash, &amounts)
                .map_err(|_| "cannot encode Tribute read results")?;
        Ok(EnclaveResponse::TributeAmountsReadV2 {
            amounts,
            inputs_canonical_hash,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })();
    response(result)
}

fn resident_chain_id(chain: B256) -> Result<u64, &'static str> {
    let chain = U256::from_be_bytes(chain.0);
    if chain > U256::from(u64::MAX) {
        return Err("resident chain id exceeds u64");
    }
    Ok(chain.to())
}

fn response(result: Result<EnclaveResponse, &'static str>) -> EnclaveResponse {
    result.unwrap_or_else(|message| EnclaveResponse::Error {
        message: message.into(),
    })
}
