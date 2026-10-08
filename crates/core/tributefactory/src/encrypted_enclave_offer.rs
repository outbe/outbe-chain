//! Verify encrypted offer responses before the factory mutates chain state.

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::{
    protocol::{EnclaveRequest, EnclaveResponse, EncryptedTributeOffer, TributeOfferStatus},
    tribute_v2::{self, EncryptedTributeOfferResultV2},
};

struct SignedOfferBatch {
    results: Vec<EncryptedTributeOfferResultV2>,
    inputs_hash: B256,
    tag: Vec<u8>,
}

pub(crate) fn process_encrypted_offers(
    chain_id: u64,
    offers: &[EncryptedTributeOffer],
) -> Result<Vec<EncryptedTributeOfferResultV2>> {
    let (key, response) = outbe_tee::try_with_enclave(|session| {
        (
            session.attestation_pub(),
            session.request(&EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 {
                offers: offers.to_vec(),
            }),
        )
    })
    .ok_or_else(|| fatal("tee_sidecar_unavailable"))?;
    let response = response.map_err(|error| fatal(format!("tee_sidecar_unavailable: {error}")))?;
    match response {
        EnclaveResponse::EncryptedTributeOfferBatchV2 {
            results,
            inputs_canonical_hash,
            attestation_tag,
        } => validate_response(
            chain_id,
            offers,
            SignedOfferBatch {
                results,
                inputs_hash: inputs_canonical_hash,
                tag: attestation_tag,
            },
            &key,
        ),
        EnclaveResponse::Error { message } => {
            Err(fatal(format!("encrypted Tribute offer enclave: {message}")))
        }
        _ => Err(fatal("unexpected encrypted Tribute offer response")),
    }
}

fn validate_response(
    chain_id: u64,
    offers: &[EncryptedTributeOffer],
    batch: SignedOfferBatch,
    key: &[u8; 32],
) -> Result<Vec<EncryptedTributeOfferResultV2>> {
    let SignedOfferBatch {
        results,
        inputs_hash,
        tag,
    } = batch;
    let expected = tribute_v2::encrypted_offer_inputs_hash(chain_id, offers);
    if inputs_hash != expected || results.len() != offers.len() {
        return Err(fatal(
            "tee_enclave_nondeterminism: encrypted offer input or count mismatch",
        ));
    }
    let preimage = tribute_v2::encrypted_offer_attestation_preimage(expected, &results)
        .map_err(|error| fatal(error.to_string()))?;
    tribute_v2::verify_attestation(key, &preimage, &tag)
        .map_err(|error| fatal(format!("tee_offer_attestation_invalid: {error}")))?;
    for (offer, result) in offers.iter().zip(&results) {
        validate_record_context(chain_id, offer, result)?;
    }
    Ok(results)
}

fn validate_record_context(
    chain_id: u64,
    offer: &EncryptedTributeOffer,
    result: &EncryptedTributeOfferResultV2,
) -> Result<()> {
    if matches!(result.status, TributeOfferStatus::Rejected { .. }) {
        if result.tribute.is_some() {
            return Err(fatal("rejected encrypted offer carries a Tribute"));
        }
        return Ok(());
    }
    let body = result
        .tribute
        .as_ref()
        .ok_or_else(|| fatal("created encrypted offer has no Tribute"))?;
    let context = &body.context;
    let expected = (
        chain_id,
        offer.worldwide_day,
        offer.tribute_currency,
        offer.reference_currency,
        offer.exclude_from_intex_issuance,
        offer
            .reference_wwd_vwap_minor
            .max(offer.reference_scurve_minor),
        tribute_v2::encrypted_offer_inputs_hash(chain_id, std::slice::from_ref(offer)),
    );
    let actual = (
        context.chain_id,
        context.worldwide_day,
        context.issuance_currency,
        context.reference_currency,
        context.exclude_from_intex_issuance,
        context.tribute_price_minor,
        context.offer_input_hash,
    );
    if !body.has_valid_encoding() || actual != expected {
        return Err(fatal(
            "tee_enclave_nondeterminism: encrypted Tribute context mismatch",
        ));
    }
    Ok(())
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_primitives::{
        time::WorldwideDay,
        tribute_encryption::{TributeAmountsV2, TributeContextV2},
    };

    struct Fixture {
        offer: EncryptedTributeOffer,
        result: EncryptedTributeOfferResultV2,
        signer: SigningKey,
    }
    impl Fixture {
        fn new() -> Self {
            let day = WorldwideDay::new(20250115);
            let owner = Address::repeat_byte(0x32);
            let offer = EncryptedTributeOffer {
                owner: Address::repeat_byte(0x31),
                cipher_text: vec![1, 2, 3],
                nonce: vec![0; 12],
                ephemeral_pubkey: U256::from(7),
                worldwide_day: day,
                tribute_currency: 840,
                reference_currency: 978,
                exclude_from_intex_issuance: false,
                issuance_wwd_vwap_minor: U256::from(100),
                reference_wwd_vwap_minor: U256::from(200),
                reference_scurve_minor: U256::from(300),
                zk_context: None,
            };
            let digest = outbe_compressed_entities::derive_poseidon_digest(owner, day).unwrap();
            let body = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
                &[7; 32],
                &outbe_tee_enclave::crypto::x25519_public(&[11; 32]),
                TributeContextV2 {
                    chain_id: 1,
                    tribute_id: outbe_compressed_entities::WwdEntityId::from_day_and_digest(
                        day, digest,
                    ),
                    owner,
                    worldwide_day: day,
                    issuance_currency: 840,
                    reference_currency: 978,
                    tribute_price_minor: U256::from(300),
                    exclude_from_intex_issuance: false,
                    offer_input_hash: tribute_v2::encrypted_offer_inputs_hash(
                        1,
                        std::slice::from_ref(&offer),
                    ),
                },
                &TributeAmountsV2 {
                    issuance_amount_minor: U256::from(100),
                    nominal_amount_minor: U256::from(50),
                },
            )
            .unwrap();
            let result = EncryptedTributeOfferResultV2 {
                token_id: digest,
                tribute: Some(body),
                su_hashes: Vec::new(),
                wallet_addresses: Vec::new(),
                sra_addresses: Vec::new(),
                zk_expected_hashes: None,
                status: TributeOfferStatus::Created,
            };
            Self {
                offer,
                result,
                signer: SigningKey::from_bytes(&[0x33; 32]),
            }
        }
        fn validate(
            &self,
            result: EncryptedTributeOfferResultV2,
        ) -> Result<Vec<EncryptedTributeOfferResultV2>> {
            let offers = std::slice::from_ref(&self.offer);
            let hash = tribute_v2::encrypted_offer_inputs_hash(1, offers);
            let results = vec![result];
            let message = tribute_v2::encrypted_offer_attestation_preimage(hash, &results).unwrap();
            let tag = self.signer.sign(&message).to_bytes();
            validate_response(
                1,
                offers,
                SignedOfferBatch {
                    results,
                    inputs_hash: hash,
                    tag: tag.to_vec(),
                },
                self.signer.verifying_key().as_bytes(),
            )
        }
    }

    #[test]
    fn signed_encrypted_result_preserves_creator_owner_distinct_from_caller() {
        let fixture = Fixture::new();
        assert_ne!(
            fixture.offer.owner,
            fixture.result.tribute.as_ref().unwrap().context.owner
        );
        assert_eq!(
            fixture.validate(fixture.result.clone()).unwrap(),
            vec![fixture.result]
        );
    }

    #[test]
    fn even_signed_results_cannot_replace_chain_resolved_context() {
        let fixture = Fixture::new();
        for mutate in [
            (|body: &mut outbe_primitives::tribute_encryption::EncryptedTributeV2| {
                body.context.chain_id += 1
            }) as fn(&mut _),
            |body| body.context.worldwide_day = WorldwideDay::new(20250116),
            |body| body.context.issuance_currency = 978,
            |body| body.context.reference_currency = 840,
            |body| body.context.tribute_price_minor += U256::ONE,
            |body| body.context.exclude_from_intex_issuance = true,
            |body| body.context.offer_input_hash = B256::ZERO,
            |body| {
                body.encrypted_amounts.pop();
            },
        ] {
            let mut result = fixture.result.clone();
            mutate(result.tribute.as_mut().unwrap());
            assert!(fixture.validate(result).is_err());
        }
    }

    #[test]
    fn wrong_signature_input_hash_count_and_missing_created_body_are_rejected() {
        let fixture = Fixture::new();
        let offers = std::slice::from_ref(&fixture.offer);
        let hash = tribute_v2::encrypted_offer_inputs_hash(1, offers);
        let key = fixture.signer.verifying_key().to_bytes();
        assert!(validate_response(
            1,
            offers,
            SignedOfferBatch {
                results: vec![fixture.result.clone()],
                inputs_hash: hash,
                tag: vec![0; 64]
            },
            &key
        )
        .is_err());
        assert!(validate_response(
            1,
            offers,
            SignedOfferBatch {
                results: vec![fixture.result.clone()],
                inputs_hash: B256::ZERO,
                tag: Vec::new()
            },
            &key
        )
        .is_err());
        assert!(validate_response(
            1,
            offers,
            SignedOfferBatch {
                results: Vec::new(),
                inputs_hash: hash,
                tag: Vec::new()
            },
            &key
        )
        .is_err());
        let mut missing = fixture.result.clone();
        missing.tribute = None;
        assert!(fixture.validate(missing).is_err());
        let mut rejected = fixture.result.clone();
        rejected.status = TributeOfferStatus::Rejected {
            reason: "fixture".into(),
        };
        assert!(fixture.validate(rejected).is_err());
    }
}
