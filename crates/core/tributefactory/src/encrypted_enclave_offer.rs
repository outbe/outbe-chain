//! Verify encrypted offer responses before the factory mutates chain state.

use outbe_primitives::error::Result;
use outbe_tee::TransportError;
use outbe_tribute::confidential::{
    encrypted_offer_inputs_hash, EncryptedTributeOffer, EncryptedTributeOfferResultV2,
    TributeOfferStatus,
};

use crate::errors::TributeFactoryError;

pub(crate) fn process_encrypted_offers(
    chain_id: u64,
    offers: &[EncryptedTributeOffer],
) -> Result<Vec<EncryptedTributeOfferResultV2>> {
    let results =
        outbe_tribute::confidential::host::process_encrypted_offer_batch(chain_id, offers.to_vec())
            .map_err(|error| -> outbe_primitives::error::PrecompileError {
                match error {
                    TransportError::EnclaveError(message) => {
                        TributeFactoryError::EncryptedOfferEnclaveError(message).into()
                    }
                    other => TributeFactoryError::sidecar_unavailable(other).into(),
                }
            })?;
    if results.len() != offers.len() {
        return Err(TributeFactoryError::EncryptedOfferInputOrCountMismatch.into());
    }
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
            return Err(TributeFactoryError::RejectedEncryptedOfferCarriesTribute.into());
        }
        return Ok(());
    }
    let body = result
        .tribute
        .as_ref()
        .ok_or(TributeFactoryError::CreatedEncryptedOfferMissingTribute)?;
    let context = &body.context;
    let expected = (
        chain_id,
        offer.worldwide_day,
        offer.tribute_currency,
        offer.reference_currency,
        offer
            .reference_wwd_vwap_minor
            .max(offer.reference_scurve_minor),
        encrypted_offer_inputs_hash(chain_id, std::slice::from_ref(offer)),
    );
    let actual = (
        context.chain_id,
        context.worldwide_day,
        context.issuance_currency,
        context.reference_currency,
        context.tribute_price_minor,
        context.offer_input_hash,
    );
    if !body.has_valid_encoding() || actual != expected {
        return Err(TributeFactoryError::EncryptedTributeContextMismatch.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use alloy_primitives::{Address, B256, U256};
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_core_common::confidential_tee::{encode_tagged_payload, WitnessResponseTag};
    use outbe_core_common::tribute_encryption::{TributeAmountsV2, TributeContextV2};
    use outbe_primitives::error::PrecompileError;
    use outbe_system_common::time::WorldwideDay;
    use outbe_tee::witness::{
        EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use outbe_tribute::confidential::encrypted_offer_attestation_preimage;
    use outbe_tribute::confidential::host::EncryptedTributeOfferBatchV2;

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
                issuance_wwd_vwap_minor: U256::from(100),
                reference_wwd_vwap_minor: U256::from(200),
                reference_scurve_minor: U256::from(300),
                zk_context: None,
            };
            let digest = outbe_core_common::wwd_entity_id::derive_poseidon_digest(owner, day);
            let body = outbe_tribute::confidential::encrypt_tribute(
                &outbe_tee::resident::StateKeyHandle::for_test([7; 32]),
                TributeContextV2 {
                    chain_id: 1,
                    tribute_id: outbe_core_common::wwd_entity_id::WwdEntityId::from_day_and_digest(
                        day, digest,
                    ),
                    owner,
                    worldwide_day: day,
                    issuance_currency: 840,
                    reference_currency: 978,
                    tribute_price_minor: U256::from(300),
                    offer_input_hash: encrypted_offer_inputs_hash(1, std::slice::from_ref(&offer)),
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
    }

    fn replay(
        offers: &[EncryptedTributeOffer],
        results: Vec<EncryptedTributeOfferResultV2>,
        inputs_hash: B256,
        tag: Vec<u8>,
        leader: &[u8; 32],
    ) -> Result<Vec<EncryptedTributeOfferResultV2>> {
        let body = EncryptedTributeOfferBatchV2 {
            results,
            inputs_canonical_hash: inputs_hash,
            attestation_tag: Vec::new(),
        };
        let payload =
            encode_tagged_payload(WitnessResponseTag::EncryptedTributeOfferBatchV2, &body)
                .expect("witness payload");
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierReplay,
            Some(*leader),
            EnclaveWitnessTapeV1 {
                prefix: vec![EnclaveWitnessEntryV1::Payload {
                    payload,
                    inputs_canonical_hash: inputs_hash,
                    attestation_tag: tag,
                }],
                txs: Vec::new(),
            },
        )));
        let _scope = WitnessExecutionScope::enter(Arc::clone(&state));
        process_encrypted_offers(1, offers)
    }

    fn sign_tag(
        signer: &SigningKey,
        inputs_hash: B256,
        results: &[EncryptedTributeOfferResultV2],
    ) -> Vec<u8> {
        let preimage =
            encrypted_offer_attestation_preimage(inputs_hash, results).expect("preimage");
        signer.sign(&preimage).to_bytes().to_vec()
    }

    fn assert_host_attestation(result: Result<Vec<EncryptedTributeOfferResultV2>>, needle: &str) {
        let Err(PrecompileError::EnclaveUnavailable(message)) = result else {
            panic!("production path accepted the tape: {result:?}");
        };
        assert!(message.contains(needle), "{message}");
        assert_ne!(
            message,
            TributeFactoryError::EncryptedOfferInputOrCountMismatch.to_string()
        );
    }

    #[test]
    fn signed_encrypted_result_preserves_creator_owner_distinct_from_caller() {
        let fixture = Fixture::new();
        assert_ne!(
            fixture.offer.owner,
            fixture.result.tribute.as_ref().unwrap().context.owner
        );
        validate_record_context(1, &fixture.offer, &fixture.result).expect("matching context");
    }

    #[test]
    fn even_signed_results_cannot_replace_chain_resolved_context() {
        let fixture = Fixture::new();
        for mutate in [
            (|body: &mut outbe_core_common::tribute_encryption::EncryptedTributeV2| {
                body.context.chain_id += 1
            }) as fn(&mut _),
            |body| body.context.worldwide_day = WorldwideDay::new(20250116),
            |body| body.context.issuance_currency = 978,
            |body| body.context.reference_currency = 840,
            |body| body.context.tribute_price_minor += U256::ONE,
            |body| body.context.offer_input_hash = B256::ZERO,
            |body| {
                body.encrypted_amounts.pop();
            },
        ] {
            let mut result = fixture.result.clone();
            mutate(result.tribute.as_mut().unwrap());
            assert!(validate_record_context(1, &fixture.offer, &result).is_err());
        }
    }

    #[test]
    fn wrong_signature_input_hash_count_and_missing_created_body_are_rejected() {
        let fixture = Fixture::new();
        let offers = std::slice::from_ref(&fixture.offer);
        let live = encrypted_offer_inputs_hash(1, offers);
        let leader = fixture.signer.verifying_key().to_bytes();
        let matched = vec![fixture.result.clone()];
        assert_host_attestation(
            replay(offers, matched.clone(), live, vec![0; 64], &leader),
            "signature invalid",
        );
        let other_hash = B256::repeat_byte(0x5A);
        assert_ne!(other_hash, live);
        assert_host_attestation(
            replay(
                offers,
                matched,
                other_hash,
                sign_tag(&fixture.signer, live, std::slice::from_ref(&fixture.result)),
                &leader,
            ),
            "inputs canonical hash mismatch",
        );
        let empty = Vec::new();
        let Err(PrecompileError::EnclaveUnavailable(message)) = replay(
            offers,
            empty.clone(),
            live,
            sign_tag(&fixture.signer, live, &empty),
            &leader,
        ) else {
            panic!("count guard returned the tape");
        };
        assert_eq!(
            message,
            TributeFactoryError::EncryptedOfferInputOrCountMismatch.to_string()
        );
        let mut missing = fixture.result.clone();
        missing.tribute = None;
        assert!(validate_record_context(1, &fixture.offer, &missing).is_err());
        let mut rejected = fixture.result.clone();
        rejected.status = TributeOfferStatus::Rejected {
            reason: "fixture".into(),
        };
        assert!(validate_record_context(1, &fixture.offer, &rejected).is_err());
    }
}
