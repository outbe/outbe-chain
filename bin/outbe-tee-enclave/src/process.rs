//! Offer-batch processing: decrypt -> validate -> select the oracle price for the
//! decrypted issuance currency -> compute the canonical public
//! `TributeOfferResult` (incl. in-enclave Poseidon `token_id`) for each offer.
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
//! Determinism: the price map is supplied by the node from committed Oracle state
//! (identical on every validator), and every step here is pure integer/hash
//! math, so all validators produce byte-identical results. A forged price
//! surfaces as a state-root mismatch on re-execution.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};

use outbe_tee::protocol::{EncryptedTributeOffer, TributeOfferResult, TributeOfferStatus};

use crate::compute::{compute_nominal, compute_token_id, normalize_amount};
use crate::crypto::ecdhe_tribute_offer_decrypt;
use crate::payload::parse_and_validate;
use crate::zk_claim::derive_expected_hashes;

/// The enclave-resident offer decryption key material (derived from the sealed
/// root seed via the HKDF chain). Borrowed for the duration of a batch call.
pub struct TributeOfferKeyMaterial<'a> {
    pub tribute_offer_private_key: &'a [u8; 32],
    pub salt: &'a [u8; 32],
}

/// Process a batch of encrypted offers against one COEN price map (`iso_code ->
/// 1e18-scaled price`) covering the whole batch. Per-offer failures become
/// `Rejected{reason}` (never abort the whole batch). Returns the results plus a
/// canonical-inputs hash used by the host to detect enclave non-determinism.
pub fn process_tribute_offer_batch(
    key: &TributeOfferKeyMaterial<'_>,
    offers: &[EncryptedTributeOffer],
    tribute_prices: &BTreeMap<u16, U256>,
) -> (Vec<TributeOfferResult>, B256) {
    let mut results = Vec::with_capacity(offers.len());
    for offer in offers {
        let result = match process_one(key, offer, tribute_prices) {
            Ok(result) => result,
            Err(reason) => rejected(offer, reason),
        };
        results.push(result);
    }
    let hash = outbe_tee::protocol::inputs_canonical_hash(offers, tribute_prices);
    (results, hash)
}

fn process_one(
    key: &TributeOfferKeyMaterial<'_>,
    offer: &EncryptedTributeOffer,
    tribute_prices: &BTreeMap<u16, U256>,
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
    let zk_expected_hashes = derive_expected_hashes(offer, &payload)?;

    let amount_minor = normalize_amount(&payload.amount_base, &payload.amount_atto)?;
    if amount_minor.is_zero() {
        return Err("amount must be positive".to_string());
    }

    // worldwide_day / currency come from the encrypted payload (authoritative), so
    // the issuance currency is only knowable here — the node ships every price it
    // could resolve and this is where one is selected. Absent means the currency is
    // unregistered or has no price for the day.
    let price = *tribute_prices
        .get(&payload.currency)
        .ok_or_else(|| format!("currency {} is not supported", payload.currency))?;
    // The map is host-supplied and `compute_nominal` divides by this, so a zero
    // is rejected here rather than trusted to the host's own filtering.
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
        exclude_from_intex_issuance: offer.exclude_from_intex_issuance,
        tribute_price_minor: price,
        // Returned for the host's SU-hash used-marking + agent-reward routing
        // (public on-chain). Privacy-preserving markers-only form is a later
        // slice (see module doc / Enclave Return Rule).
        su_hashes: payload.su_hashes,
        wallet_addresses: payload.wallet_addresses,
        sra_addresses: payload.sra_addresses,
        zk_expected_hashes,
        status: TributeOfferStatus::Created,
    })
}

/// Build a `Rejected` result from the offer's public (non-decrypted) fields.
/// `token_id`/`worldwide_day`/`issuance_currency`/`tribute_price_minor` are
/// unknown (the price is selected by the decrypted currency, which decryption may
/// not have reached); `owner` is the public sender and is always known.
fn rejected(offer: &EncryptedTributeOffer, reason: String) -> TributeOfferResult {
    TributeOfferResult {
        token_id: B256::ZERO,
        owner: offer.owner,
        worldwide_day: 0,
        issuance_amount_minor: U256::ZERO,
        issuance_currency: 0,
        nominal_amount_minor: U256::ZERO,
        reference_currency: offer.reference_currency,
        exclude_from_intex_issuance: offer.exclude_from_intex_issuance,
        tribute_price_minor: U256::ZERO,
        su_hashes: Vec::new(),
        wallet_addresses: Vec::new(),
        sra_addresses: Vec::new(),
        zk_expected_hashes: None,
        status: TributeOfferStatus::Rejected { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::{compute_token_id, SCALE_1E18};
    use crate::crypto::{chacha20poly1305_encrypt, hkdf_sha256};
    use alloy_primitives::Address;
    use outbe_tee::protocol::TributeZkContext;
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
            exclude_from_intex_issuance: false,
            zk_context: None,
        }
    }

    /// A batch price map holding a single USD entry.
    fn usd_prices(price: U256) -> BTreeMap<u16, U256> {
        BTreeMap::from([(840u16, price)])
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
        let offers = vec![make_tribute_offer(owner, GOOD_JSON, 840)];

        let (results, hash) = process_tribute_offer_batch(&key(), &offers, &usd_prices(price));
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
    fn zk_enabled_offer_returns_expected_hashes_bound_to_owner_and_sender() {
        let sender = Address::repeat_byte(0xAB);
        let mut offer = make_tribute_offer(sender, GOOD_JSON, 840);
        offer.zk_context = Some(TributeZkContext {
            derived_owner: B256::from([0x01; 32]),
            chain_id: 19_280_501,
        });

        let (results, _) =
            process_tribute_offer_batch(&key(), &[offer.clone()], &usd_prices(SCALE_1E18));
        let expected = results[0]
            .zk_expected_hashes
            .as_ref()
            .expect("ZK context must produce expected hashes");
        assert_ne!(expected.nft_hash, B256::ZERO);
        assert_ne!(expected.binding_hash, B256::ZERO);

        offer.zk_context.as_mut().unwrap().derived_owner = B256::from([0x02; 32]);
        let (different_owner, _) =
            process_tribute_offer_batch(&key(), &[offer.clone()], &usd_prices(SCALE_1E18));
        assert_ne!(
            different_owner[0]
                .zk_expected_hashes
                .as_ref()
                .unwrap()
                .nft_hash,
            expected.nft_hash
        );

        offer.owner = Address::repeat_byte(0xCD);
        let (different_sender, _) =
            process_tribute_offer_batch(&key(), &[offer], &usd_prices(SCALE_1E18));
        assert_ne!(
            different_sender[0]
                .zk_expected_hashes
                .as_ref()
                .unwrap()
                .binding_hash,
            expected.binding_hash
        );
    }

    #[test]
    fn zk_nft_hash_uses_canonical_su_id_set_order() {
        let first = GOOD_JSON.replace(
            r#""su_hashes": ["0x2222222222222222222222222222222222222222222222222222222222222222"]"#,
            r#""su_hashes": [
                "0x2323232323232323232323232323232323232323232323232323232323232323",
                "0x2222222222222222222222222222222222222222222222222222222222222222"
            ]"#,
        );
        let second = GOOD_JSON.replace(
            r#""su_hashes": ["0x2222222222222222222222222222222222222222222222222222222222222222"]"#,
            r#""su_hashes": [
                "0x2222222222222222222222222222222222222222222222222222222222222222",
                "0x2323232323232323232323232323232323232323232323232323232323232323"
            ]"#,
        );
        let mut first = make_tribute_offer(Address::repeat_byte(0xAB), &first, 840);
        let mut second = make_tribute_offer(Address::repeat_byte(0xAB), &second, 840);
        let context = TributeZkContext {
            derived_owner: B256::from([0x01; 32]),
            chain_id: 19_280_501,
        };
        first.zk_context = Some(context.clone());
        second.zk_context = Some(context);

        let (results, _) =
            process_tribute_offer_batch(&key(), &[first, second], &usd_prices(SCALE_1E18));
        assert_eq!(
            results[0].zk_expected_hashes.as_ref().unwrap().nft_hash,
            results[1].zk_expected_hashes.as_ref().unwrap().nft_hash
        );
    }

    #[test]
    fn zk_enabled_offer_rejects_noncanonical_atto_amount() {
        let json = GOOD_JSON.replace(
            r#""amount_atto": "0""#,
            r#""amount_atto": "1000000000000000000""#,
        );
        let mut offer = make_tribute_offer(Address::repeat_byte(0xAB), &json, 840);
        offer.zk_context = Some(TributeZkContext {
            derived_owner: B256::from([0x01; 32]),
            chain_id: 19_280_501,
        });

        let (results, _) = process_tribute_offer_batch(&key(), &[offer], &usd_prices(SCALE_1E18));
        assert!(matches!(
            &results[0].status,
            TributeOfferStatus::Rejected { reason }
                if reason.contains("amount_atto must be less than 1e18")
        ));
    }

    #[test]
    fn inputs_hash_binds_zk_owner_and_chain_id() {
        let mut offer = make_tribute_offer(Address::repeat_byte(0xAB), GOOD_JSON, 840);
        offer.zk_context = Some(TributeZkContext {
            derived_owner: B256::from([0x01; 32]),
            chain_id: 19_280_501,
        });
        let original =
            outbe_tee::protocol::inputs_canonical_hash(&[offer.clone()], &usd_prices(SCALE_1E18));

        offer.zk_context.as_mut().unwrap().derived_owner = B256::from([0x02; 32]);
        let different_owner =
            outbe_tee::protocol::inputs_canonical_hash(&[offer.clone()], &usd_prices(SCALE_1E18));
        assert_ne!(original, different_owner);

        offer.zk_context.as_mut().unwrap().chain_id += 1;
        let different_chain =
            outbe_tee::protocol::inputs_canonical_hash(&[offer], &usd_prices(SCALE_1E18));
        assert_ne!(different_owner, different_chain);
    }

    /// The unencrypted `exclude_from_intex_issuance` ABI flag is echoed straight
    /// back on the result (like `reference_currency`) — both on the created path
    /// and, defensively, on the rejected path.
    #[test]
    fn exclude_from_intex_issuance_is_echoed() {
        let owner = Address::repeat_byte(0xC1);
        let price = U256::from(2u64) * SCALE_1E18;
        let mut offer = make_tribute_offer(owner, GOOD_JSON, 840);
        offer.exclude_from_intex_issuance = true;

        let (results, _) =
            process_tribute_offer_batch(&key(), &[offer.clone()], &usd_prices(price));
        assert_eq!(results[0].status, TributeOfferStatus::Created);
        assert!(results[0].exclude_from_intex_issuance);

        // Rejected path (unpriced currency) still carries the flag.
        let (rejected, _) = process_tribute_offer_batch(&key(), &[offer], &BTreeMap::new());
        assert!(matches!(
            rejected[0].status,
            TributeOfferStatus::Rejected { .. }
        ));
        assert!(rejected[0].exclude_from_intex_issuance);
    }

    /// A host that hands over a zero price must not reach the division in
    /// `compute_nominal`; the offer is rejected and the batch continues.
    #[test]
    fn zero_price_is_rejected_not_aborted() {
        // Distinct owners so both offers are independent (one owner = at most one
        // Tribute per day).
        let owner_a = Address::repeat_byte(0x01);
        let owner_b = Address::repeat_byte(0x0B);
        let offers = vec![
            make_tribute_offer(owner_a, GOOD_JSON, 840),
            make_tribute_offer(owner_b, GOOD_JSON, 840),
        ];
        let (results, _) = process_tribute_offer_batch(&key(), &offers, &usd_prices(U256::ZERO));
        assert!(matches!(
            results[0].status,
            TributeOfferStatus::Rejected { .. }
        ));
        assert!(matches!(
            results[1].status,
            TributeOfferStatus::Rejected { .. }
        ));

        let (results, _) = process_tribute_offer_batch(&key(), &offers, &usd_prices(SCALE_1E18));
        assert_eq!(results[0].status, TributeOfferStatus::Created);
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
            exclude_from_intex_issuance: false,
            zk_context: None,
        };
        let (results, _) = process_tribute_offer_batch(&key(), &[offer], &usd_prices(SCALE_1E18));
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
        )];
        let (results, _) = process_tribute_offer_batch(&key(), &offers, &usd_prices(SCALE_1E18));
        assert!(matches!(
            results[0].status,
            TributeOfferStatus::Rejected { .. }
        ));
    }

    #[test]
    fn inputs_hash_is_deterministic_and_input_bound() {
        let owner = Address::repeat_byte(0x03);
        let offers = vec![make_tribute_offer(owner, GOOD_JSON, 840)];
        let prices = usd_prices(SCALE_1E18);
        let h1 = outbe_tee::protocol::inputs_canonical_hash(&offers, &prices);
        let h2 = outbe_tee::protocol::inputs_canonical_hash(&offers, &prices);
        assert_eq!(h1, h2);
        // different reference currency -> different hash
        let offers2 = vec![make_tribute_offer(owner, GOOD_JSON, 978)];
        assert_ne!(
            h1,
            outbe_tee::protocol::inputs_canonical_hash(&offers2, &prices)
        );
    }

    /// The price map is a batch input, so it has to move the canonical hash — both
    /// when an entry's price changes and when a currency is added. Without this
    /// the map would ride to the enclave unhashed and unattested.
    #[test]
    fn inputs_hash_binds_the_price_map() {
        let offers = vec![make_tribute_offer(
            Address::repeat_byte(0x05),
            GOOD_JSON,
            840,
        )];
        let base = outbe_tee::protocol::inputs_canonical_hash(&offers, &usd_prices(SCALE_1E18));

        let changed_price = outbe_tee::protocol::inputs_canonical_hash(
            &offers,
            &usd_prices(U256::from(2u64) * SCALE_1E18),
        );
        assert_ne!(base, changed_price);

        let extra_currency = outbe_tee::protocol::inputs_canonical_hash(
            &offers,
            &BTreeMap::from([(840u16, SCALE_1E18), (978u16, SCALE_1E18)]),
        );
        assert_ne!(base, extra_currency);
        assert_ne!(changed_price, extra_currency);
    }

    /// A currency absent from the batch map is unsupported: the offer is rejected
    /// by name, and the rest of the batch is unaffected.
    #[test]
    fn unsupported_currency_is_rejected_not_aborted() {
        let eur_json = GOOD_JSON.replace(r#""currency": 840"#, r#""currency": 978"#);
        let offers = vec![
            make_tribute_offer(Address::repeat_byte(0x01), &eur_json, 840),
            make_tribute_offer(Address::repeat_byte(0x0B), GOOD_JSON, 840),
        ];

        let (results, _) = process_tribute_offer_batch(&key(), &offers, &usd_prices(SCALE_1E18));
        assert!(matches!(
            &results[0].status,
            TributeOfferStatus::Rejected { reason }
                if reason.contains("currency 978 is not supported")
        ));
        assert_eq!(results[1].status, TributeOfferStatus::Created);
    }

    /// Regression for the removed USD pin: a non-840 issuance currency prices and
    /// issues normally once the map carries it.
    #[test]
    fn non_usd_currency_is_priced_from_the_map() {
        let eur_json = GOOD_JSON.replace(r#""currency": 840"#, r#""currency": 978"#);
        let owner = Address::repeat_byte(0xE7);
        let eur_price = U256::from(4u64) * SCALE_1E18;
        let prices = BTreeMap::from([(840u16, U256::from(2u64) * SCALE_1E18), (978u16, eur_price)]);

        let offers = vec![make_tribute_offer(owner, &eur_json, 840)];
        let (results, _) = process_tribute_offer_batch(&key(), &offers, &prices);

        let r = &results[0];
        assert_eq!(r.status, TributeOfferStatus::Created);
        assert_eq!(r.issuance_currency, 978);
        assert_eq!(r.tribute_price_minor, eur_price);
        // Priced off the 978 entry, not the 840 one: 100e18 * 1e18 / 4e18 = 25e18.
        assert_eq!(r.nominal_amount_minor, U256::from(25u64) * SCALE_1E18);
        // The public reference currency is a separate axis and is echoed unchanged.
        assert_eq!(r.reference_currency, 840);
    }

    /// Two offers in one batch, two issuance currencies, one map — the case the
    /// old single-scalar wire could not express.
    #[test]
    fn one_batch_prices_each_offer_in_its_own_currency() {
        let eur_json = GOOD_JSON.replace(r#""currency": 840"#, r#""currency": 978"#);
        let usd_price = U256::from(2u64) * SCALE_1E18;
        let eur_price = U256::from(5u64) * SCALE_1E18;
        let prices = BTreeMap::from([(840u16, usd_price), (978u16, eur_price)]);

        let offers = vec![
            make_tribute_offer(Address::repeat_byte(0x01), GOOD_JSON, 840),
            make_tribute_offer(Address::repeat_byte(0x0B), &eur_json, 840),
        ];
        let (results, _) = process_tribute_offer_batch(&key(), &offers, &prices);

        assert_eq!(results[0].status, TributeOfferStatus::Created);
        assert_eq!(results[0].issuance_currency, 840);
        assert_eq!(results[0].tribute_price_minor, usd_price);

        assert_eq!(results[1].status, TributeOfferStatus::Created);
        assert_eq!(results[1].issuance_currency, 978);
        assert_eq!(results[1].tribute_price_minor, eur_price);
    }
}
