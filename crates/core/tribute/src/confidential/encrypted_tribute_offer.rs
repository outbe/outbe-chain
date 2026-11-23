//! V2 offer processing: creator ownership and encrypted canonical amounts.

use alloy_primitives::{Address, B256};
use outbe_core_common::tribute_encryption::{TributeAmountsV2, TributeContextV2};
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_tee::resident::StateKeyHandle;

use super::errors::{display_text, OfferError, OfferResult, ProcessError};
use super::process::TributeOfferKeyMaterial;
use super::protocol::{EncryptedTributeOffer, TributeOfferStatus};
use super::tribute_encryption::encrypt_tribute;
use super::tribute_v2::{encrypted_offer_inputs_hash, EncryptedTributeOfferResultV2};
use outbe_nod::offer::compute::{compute_nominal, compute_token_id, parse_canonical_amount};
use outbe_nod::offer::zk_claim::derive_expected_hashes;

/// Process one encrypted batch. A single offer failure stays `Rejected`.
pub fn process_encrypted_tribute_offer_batch(
    key: &TributeOfferKeyMaterial<'_>,
    tribute_state_key: &StateKeyHandle,
    chain_id: u64,
    offers: &[EncryptedTributeOffer],
) -> (Vec<EncryptedTributeOfferResultV2>, B256) {
    let results = offers
        .iter()
        .map(|offer| {
            process_one(key, tribute_state_key, chain_id, offer)
                .unwrap_or_else(|err| EncryptedTributeOfferResultV2::rejected(display_text(&err)))
        })
        .collect();
    (results, encrypted_offer_inputs_hash(chain_id, offers))
}

fn process_one(
    key: &TributeOfferKeyMaterial<'_>,
    tribute_state_key: &StateKeyHandle,
    chain_id: u64,
    offer: &EncryptedTributeOffer,
) -> OfferResult<EncryptedTributeOfferResultV2> {
    if offer
        .zk_context
        .as_ref()
        .is_some_and(|context| context.chain_id != chain_id)
    {
        return Err(ProcessError::OfferProofChainMismatch.into());
    }
    let payload = outbe_nod::offer::open_offer(key, offer)?;
    let owner = payload
        .creator
        .parse::<Address>()
        .map_err(|_| OfferError::from(ProcessError::InvalidCreatorAddress))?;
    let amount = parse_canonical_amount(&payload.amount_base, &payload.amount_micro)?;
    if amount.amount_minor.is_zero() {
        return Err(ProcessError::AmountNotPositive.into());
    }
    let zk_expected_hashes = derive_expected_hashes(offer, &payload, &amount)?;
    let (nominal_amount_minor, tribute_price_minor) = compute_nominal(
        amount.amount_minor,
        offer.issuance_wwd_vwap_minor,
        offer.reference_wwd_vwap_minor,
        offer.reference_scurve_minor,
    )?;
    let token_id = compute_token_id(owner, offer.worldwide_day, &payload.tribute_draft_id)?;
    let context = TributeContextV2 {
        chain_id,
        tribute_id: WwdEntityId::from_day_and_digest(offer.worldwide_day, token_id),
        owner,
        worldwide_day: offer.worldwide_day,
        issuance_currency: offer.tribute_currency,
        reference_currency: offer.reference_currency,
        tribute_price_minor,
        offer_input_hash: encrypted_offer_inputs_hash(chain_id, std::slice::from_ref(offer)),
    };
    let amounts = TributeAmountsV2 {
        issuance_amount_minor: amount.amount_minor,
        nominal_amount_minor,
    };
    let tribute = encrypt_tribute(tribute_state_key, context, &amounts)
        .map_err(ProcessError::TributeEncryptionFailed)?;
    Ok(EncryptedTributeOfferResultV2 {
        token_id,
        tribute: Some(tribute),
        su_hashes: payload.su_hashes,
        wallet_addresses: payload.wallet_addresses,
        sra_addresses: payload.sra_addresses,
        zk_expected_hashes,
        status: TributeOfferStatus::Created,
    })
}
