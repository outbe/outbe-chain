//! V2 offer processing: creator ownership and encrypted canonical amounts.

use alloy_primitives::{Address, B256};
use outbe_primitives::{
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};
use outbe_tee::{
    protocol::{EncryptedTributeOffer, TributeOfferStatus},
    tribute_v2::{encrypted_offer_inputs_hash, EncryptedTributeOfferResultV2},
};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{
    compute::{compute_nominal, compute_token_id, parse_canonical_amount},
    crypto::ecdhe_tribute_offer_decrypt,
    payload::{self, TributeInputPayload},
    process::TributeOfferKeyMaterial,
    tribute_encryption::encrypt_tribute,
    zk_claim::derive_expected_hashes,
};

#[derive(Deserialize)]
struct PayloadV2 {
    #[serde(flatten)]
    input: TributeInputPayload,
    creator_public_key: B256,
}

pub fn process_encrypted_tribute_offer_batch(
    key: &TributeOfferKeyMaterial<'_>,
    chain_id: u64,
    offers: &[EncryptedTributeOffer],
) -> (Vec<EncryptedTributeOfferResultV2>, B256) {
    let results = offers
        .iter()
        .map(|offer| {
            process_one(key, chain_id, offer)
                .unwrap_or_else(EncryptedTributeOfferResultV2::rejected)
        })
        .collect();
    (results, encrypted_offer_inputs_hash(chain_id, offers))
}

fn decrypt_payload(
    key: &TributeOfferKeyMaterial<'_>,
    offer: &EncryptedTributeOffer,
) -> Result<PayloadV2, String> {
    let plaintext = Zeroizing::new(
        ecdhe_tribute_offer_decrypt(
            key.tribute_offer_private_key,
            key.salt,
            &offer.ephemeral_pubkey.to_be_bytes::<32>(),
            &offer.nonce,
            &offer.cipher_text,
        )
        .map_err(|error| format!("decryption failed: {error}"))?,
    );
    let payload: PayloadV2 = serde_json::from_slice(&plaintext)
        .map_err(|error| format!("invalid encrypted Tribute payload: {error}"))?;
    payload::validate(&payload.input)?;
    Ok(payload)
}

fn process_one(
    key: &TributeOfferKeyMaterial<'_>,
    chain_id: u64,
    offer: &EncryptedTributeOffer,
) -> Result<EncryptedTributeOfferResultV2, String> {
    if offer
        .zk_context
        .as_ref()
        .is_some_and(|context| context.chain_id != chain_id)
    {
        return Err("offer proof chain differs from resident chain".into());
    }
    let payload = decrypt_payload(key, offer)?;
    let owner = payload
        .input
        .creator
        .parse::<Address>()
        .map_err(|_| "creator must be an L1 address")?;
    let amount = parse_canonical_amount(&payload.input.amount_base, &payload.input.amount_micro)?;
    if amount.amount_minor.is_zero() {
        return Err("amount must be positive".into());
    }
    let zk_expected_hashes = derive_expected_hashes(offer, &payload.input, &amount)?;
    let (nominal_amount_minor, tribute_price_minor) = compute_nominal(
        amount.amount_minor,
        offer.issuance_wwd_vwap_minor,
        offer.reference_wwd_vwap_minor,
        offer.reference_scurve_minor,
    )?;
    let token_id = compute_token_id(owner, offer.worldwide_day, &payload.input.tribute_draft_id)?;
    let context = TributeContextV2 {
        chain_id,
        tribute_id: WwdEntityId::from_day_and_digest(offer.worldwide_day, token_id),
        owner,
        worldwide_day: offer.worldwide_day,
        issuance_currency: offer.tribute_currency,
        reference_currency: offer.reference_currency,
        tribute_price_minor,
        exclude_from_intex_issuance: offer.exclude_from_intex_issuance,
        offer_input_hash: encrypted_offer_inputs_hash(chain_id, std::slice::from_ref(offer)),
    };
    let amounts = TributeAmountsV2 {
        issuance_amount_minor: amount.amount_minor,
        nominal_amount_minor,
    };
    let tribute = encrypt_tribute(
        key.tribute_offer_private_key,
        &payload.creator_public_key.0,
        context,
        &amounts,
    )
    .map_err(|error| format!("Tribute encryption failed: {error}"))?;
    Ok(EncryptedTributeOfferResultV2 {
        token_id,
        tribute: Some(tribute),
        su_hashes: payload.input.su_hashes,
        wallet_addresses: payload.input.wallet_addresses,
        sra_addresses: payload.input.sra_addresses,
        zk_expected_hashes,
        status: TributeOfferStatus::Created,
    })
}
