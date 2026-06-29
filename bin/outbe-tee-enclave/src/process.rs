//! Offer-batch processing: decrypt -> validate -> apply oracle price -> compute
//! the canonical public `TributeOfferResult` (incl. in-enclave Poseidon
//! `token_id`) for each offer.
//!
//! What the enclave does NOT do (stays on the host):
//!   - worldwide-day OFFERING status check (needs chain state);
//!   - tribute-already-exists check;
//!   - SU-hash used-marking (replay prevention);
//!   - agent-reward (wallet/SRA) increments.
//!
//! The host applies those after receiving the public results. SU-hash markers
//! and agent-reward routing in a privacy-preserving form are a later slice.
//!
//! Determinism: the price is supplied by the node from committed Oracle state
//! (identical on every validator), and every step here is pure integer/hash
//! math, so all validators produce byte-identical results. A forged price
//! surfaces as a state-root mismatch on re-execution.

use alloy_primitives::{B256, U256};

use outbe_tee::protocol::{EncryptedTributeOffer, TributeOfferResult, TributeOfferStatus};

use crate::compute::{check_currency, compute_nominal, compute_token_id, normalize_amount};
use crate::crypto::ecdhe_tribute_offer_decrypt;
use crate::payload::parse_and_validate;

/// The enclave-resident offer decryption key material (derived from the sealed
/// root seed via the HKDF chain). Borrowed for the duration of a batch call.
pub struct TributeOfferKeyMaterial<'a> {
    pub tribute_offer_private_key: &'a [u8; 32],
    pub salt: &'a [u8; 32],
}

/// Process a batch of encrypted offers. Each offer is self-contained (carries
/// its own `owner`, `reference_currency`, and oracle price). Per-offer failures
/// become `Rejected{reason}` (never abort the whole batch). Returns the results
/// plus a canonical-inputs hash used by the host to detect enclave
/// non-determinism.
pub fn process_tribute_offer_batch(
    key: &TributeOfferKeyMaterial<'_>,
    offers: &[EncryptedTributeOffer],
) -> (Vec<TributeOfferResult>, B256) {
    let mut results = Vec::with_capacity(offers.len());
    for offer in offers {
        let result = match process_one(key, offer) {
            Ok(result) => result,
            Err(reason) => rejected(offer, reason),
        };
        results.push(result);
    }
    let hash = outbe_tee::protocol::inputs_canonical_hash(offers);
    (results, hash)
}

fn process_one(
    key: &TributeOfferKeyMaterial<'_>,
    offer: &EncryptedTributeOffer,
) -> Result<TributeOfferResult, String> {
    let ephemeral = offer.ephemeral_pubkey.to_be_bytes::<32>();
    let plaintext = ecdhe_tribute_offer_decrypt(
        key.tribute_offer_private_key,
        key.salt,
        &ephemeral,
        &offer.nonce,
        &offer.cipher_text,
    )
    .map_err(|e| format!("decryption failed: {e}"))?;

    let payload = parse_and_validate(&plaintext)?;

    // worldwide_day / currency come from the encrypted payload (authoritative);
    // the node already read the current USDC/COEN rate and passed it in.
    check_currency(offer.reference_currency)?;
    check_currency(payload.currency)?;

    let amount_minor = normalize_amount(&payload.amount_base, &payload.amount_atto)?;
    if amount_minor.is_zero() {
        return Err("amount must be positive".to_string());
    }

    let price = offer.tribute_price_minor;
    if price.is_zero() {
        return Err(format!(
            "nominal price unavailable for worldwide_day {}",
            payload.worldwide_day
        ));
    }

    let nominal_amount_minor = compute_nominal(amount_minor, price)?;

    // token_id is Poseidon over the authoritative (decrypted) owner + day. It is
    // deterministic in (owner, worldwide_day) so a duplicate offer for the same
    // owner and day collides and is rejected downstream (TributeAlreadyExists).
    // draft_id is still validated by compute_token_id but not bound into the id.
    let token_id = compute_token_id(
        offer.owner,
        payload.worldwide_day,
        &payload.tribute_draft_id,
    )?;

    Ok(TributeOfferResult {
        token_id,
        owner: offer.owner,
        worldwide_day: payload.worldwide_day,
        issuance_amount_minor: amount_minor,
        issuance_currency: payload.currency,
        nominal_amount_minor,
        reference_currency: offer.reference_currency,
        tribute_price_minor: price,
        // Returned for the host's SU-hash used-marking + agent-reward routing
        // (public on-chain). Privacy-preserving markers-only form is a later
        // slice (see module doc / Enclave Return Rule).
        su_hashes: payload.su_hashes,
        wallet_addresses: payload.wallet_addresses,
        sra_addresses: payload.sra_addresses,
        status: TributeOfferStatus::Created,
    })
}

/// Build a `Rejected` result from the offer's public (non-decrypted) fields.
/// `token_id`/`worldwide_day`/`issuance_currency` are unknown (decryption may
/// have failed); `owner` is the public sender and is always known.
fn rejected(offer: &EncryptedTributeOffer, reason: String) -> TributeOfferResult {
    TributeOfferResult {
        token_id: B256::ZERO,
        owner: offer.owner,
        worldwide_day: 0,
        issuance_amount_minor: U256::ZERO,
        issuance_currency: 0,
        nominal_amount_minor: U256::ZERO,
        reference_currency: offer.reference_currency,
        tribute_price_minor: offer.tribute_price_minor,
        su_hashes: Vec::new(),
        wallet_addresses: Vec::new(),
        sra_addresses: Vec::new(),
        status: TributeOfferStatus::Rejected { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::{compute_token_id, SCALE_1E18};
    use crate::crypto::{chacha20poly1305_encrypt, hkdf_sha256};
    use alloy_primitives::Address;
    use x25519_dalek::{PublicKey, StaticSecret};

    const OFFER_SK: [u8; 32] = [7u8; 32];
    const SALT: [u8; 32] = [3u8; 32];
    const NONCE: [u8; 12] = [1u8; 12];
    const DRAFT: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    /// Encrypt a payload the way a client would (ephemeral_secret x tribute_offer_pub).
    fn make_tribute_offer(
        owner: Address,
        json: &str,
        reference_currency: u16,
        price: U256,
    ) -> EncryptedTributeOffer {
        let tribute_offer_pub = PublicKey::from(&StaticSecret::from(OFFER_SK)).to_bytes();
        let eph_sk = [9u8; 32];
        let eph_pub = PublicKey::from(&StaticSecret::from(eph_sk)).to_bytes();
        let shared = StaticSecret::from(eph_sk).diffie_hellman(&PublicKey::from(tribute_offer_pub));
        let key = hkdf_sha256(&SALT, shared.as_bytes(), b"tribute-factory-encryption").unwrap();
        let ciphertext = chacha20poly1305_encrypt(&key, &NONCE, json.as_bytes()).unwrap();
        EncryptedTributeOffer {
            owner,
            cipher_text: ciphertext,
            nonce: NONCE.to_vec(),
            ephemeral_pubkey: U256::from_be_bytes(eph_pub),
            reference_currency,
            tribute_price_minor: price,
        }
    }

    fn key() -> TributeOfferKeyMaterial<'static> {
        TributeOfferKeyMaterial {
            tribute_offer_private_key: &OFFER_SK,
            salt: &SALT,
        }
    }

    const GOOD_JSON: &str = r#"{
        "creator": "alice",
        "tribute_draft_id": "0x1111111111111111111111111111111111111111111111111111111111111111",
        "worldwide_day": 20250115,
        "currency": 840,
        "amount_base": "100",
        "amount_atto": "0",
        "su_hashes": ["0x2222222222222222222222222222222222222222222222222222222222222222"]
    }"#;

    #[test]
    fn batch_creates_tribute_with_correct_economics() {
        let owner = Address::repeat_byte(0xAB);
        let price = U256::from(2u64) * SCALE_1E18; // 2.0
        let offers = vec![make_tribute_offer(owner, GOOD_JSON, 840, price)];

        let (results, hash) = process_tribute_offer_batch(&key(), &offers);
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.status, TributeOfferStatus::Created);
        assert_eq!(r.owner, owner);
        assert_eq!(r.worldwide_day, 20250115);
        assert_eq!(r.issuance_amount_minor, U256::from(100u64) * SCALE_1E18);
        assert_eq!(r.issuance_currency, 840);
        // 100e18 * 1e18 / 2e18 = 50e18
        assert_eq!(r.nominal_amount_minor, U256::from(50u64) * SCALE_1E18);
        assert_eq!(r.reference_currency, 840);
        assert_eq!(
            r.token_id,
            compute_token_id(owner, 20250115, DRAFT).unwrap()
        );
        assert_ne!(hash, B256::ZERO);
    }

    #[test]
    fn zero_price_is_rejected_not_aborted() {
        // Distinct owners so both offers are independent (one owner = at most one
        // Tribute per day); only the zero-price one is rejected.
        let owner_a = Address::repeat_byte(0x01);
        let owner_b = Address::repeat_byte(0x0B);
        let offers = vec![
            make_tribute_offer(owner_a, GOOD_JSON, 840, U256::ZERO), // bad: zero price
            make_tribute_offer(owner_b, GOOD_JSON, 840, SCALE_1E18), // good
        ];
        let (results, _) = process_tribute_offer_batch(&key(), &offers);
        assert!(matches!(
            results[0].status,
            TributeOfferStatus::Rejected { .. }
        ));
        assert_eq!(results[1].status, TributeOfferStatus::Created);
    }

    #[test]
    fn garbage_ciphertext_is_rejected() {
        let offer = EncryptedTributeOffer {
            owner: Address::repeat_byte(0x02),
            cipher_text: vec![0xDE, 0xAD, 0xBE, 0xEF],
            nonce: NONCE.to_vec(),
            ephemeral_pubkey: U256::ZERO,
            reference_currency: 840,
            tribute_price_minor: SCALE_1E18,
        };
        let (results, _) = process_tribute_offer_batch(&key(), &[offer]);
        assert!(matches!(
            results[0].status,
            TributeOfferStatus::Rejected { .. }
        ));
    }

    #[test]
    fn bad_draft_id_is_rejected() {
        let bad_json = r#"{
            "creator": "alice",
            "tribute_draft_id": "not-a-32-byte-hex",
            "worldwide_day": 20250115,
            "currency": 840,
            "amount_base": "100",
            "amount_atto": "0",
            "su_hashes": ["0x2222222222222222222222222222222222222222222222222222222222222222"]
        }"#;
        let offers = vec![make_tribute_offer(
            Address::repeat_byte(0x04),
            bad_json,
            840,
            SCALE_1E18,
        )];
        let (results, _) = process_tribute_offer_batch(&key(), &offers);
        assert!(matches!(
            results[0].status,
            TributeOfferStatus::Rejected { .. }
        ));
    }

    #[test]
    fn inputs_hash_is_deterministic_and_input_bound() {
        let owner = Address::repeat_byte(0x03);
        let offers = vec![make_tribute_offer(owner, GOOD_JSON, 840, SCALE_1E18)];
        let h1 = outbe_tee::protocol::inputs_canonical_hash(&offers);
        let h2 = outbe_tee::protocol::inputs_canonical_hash(&offers);
        assert_eq!(h1, h2);
        // different reference currency -> different hash
        let offers2 = vec![make_tribute_offer(owner, GOOD_JSON, 978, SCALE_1E18)];
        assert_ne!(h1, outbe_tee::protocol::inputs_canonical_hash(&offers2));
    }
}
