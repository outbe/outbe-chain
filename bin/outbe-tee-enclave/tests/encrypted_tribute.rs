use alloy_primitives::{Address, B256, U256};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TributeContextV2,
};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_tee::protocol::{EncryptedTributeOffer, TributeOfferStatus, TributeZkContext};
use outbe_tee::tribute_decrypt::decrypt_tribute_for_owner;
use outbe_tee_enclave::encrypted_tribute_offer::process_encrypted_tribute_offer_batch;
use outbe_tee_enclave::process::TributeOfferKeyMaterial;
use outbe_tee_enclave::tribute_encryption::{decrypt_tribute, derive_view_key, encrypt_tribute};
use x25519_dalek::{PublicKey, StaticSecret};

struct Fixture {
    tribute_state_key: [u8; 32],
    view_key: [u8; 32],
    enclave_public: [u8; 32],
    encrypted: EncryptedTributeV2,
    amounts: TributeAmountsV2,
}

fn fixture() -> Fixture {
    let tribute_state_key = [7u8; 32];
    let enclave_public = PublicKey::from(&StaticSecret::from(tribute_state_key)).to_bytes();
    let day = WorldwideDay::new(20250115);
    let owner = Address::repeat_byte(0x33);
    let view_key = *derive_view_key(&tribute_state_key, owner).unwrap();
    let context = TributeContextV2 {
        chain_id: 54322345,
        tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(0x22)),
        owner,
        worldwide_day: day,
        issuance_currency: 840,
        reference_currency: 978,
        tribute_price_minor: U256::from(2_000_000),
        offer_input_hash: B256::repeat_byte(0x44),
    };
    let amounts = TributeAmountsV2 {
        issuance_amount_minor: U256::from(100_000_000),
        nominal_amount_minor: U256::from(50_000_000),
    };
    let encrypted = encrypt_tribute(&tribute_state_key, context, &amounts).unwrap();
    Fixture {
        tribute_state_key,
        view_key,
        enclave_public,
        encrypted,
        amounts,
    }
}

#[test]
fn creator_and_enclave_can_read_the_same_encrypted_tribute_without_a_saved_amount_key() {
    let Fixture {
        tribute_state_key,
        view_key,
        encrypted,
        amounts,
        ..
    } = fixture();

    assert_eq!(encrypted.encrypted_amounts.len(), 88);
    assert_eq!(
        decrypt_tribute(&tribute_state_key, &encrypted).unwrap(),
        amounts
    );
    assert_eq!(
        decrypt_tribute_for_owner(&view_key, &encrypted).unwrap(),
        amounts
    );
    assert_eq!(
        decrypt_tribute(&tribute_state_key, &encrypted).unwrap(),
        amounts
    );
}

fn assert_unreadable(f: &Fixture, encrypted: &EncryptedTributeV2) {
    assert!(decrypt_tribute(&f.tribute_state_key, encrypted).is_err());
    assert!(decrypt_tribute_for_owner(&f.view_key, encrypted).is_err());
}

#[test]
fn ciphertext_and_metadata_cannot_be_substituted_between_tributes() {
    let f = fixture();
    let changes: &[fn(&mut TributeContextV2)] = &[
        |c| c.chain_id += 1,
        |c| c.owner = Address::repeat_byte(0x55),
        |c| c.worldwide_day = WorldwideDay::new(20250116),
        |c| {
            c.tribute_id =
                WwdEntityId::from_day_and_digest(c.worldwide_day, B256::repeat_byte(0x66))
        },
        |c| c.issuance_currency = 392,
        |c| c.reference_currency = 826,
        |c| c.tribute_price_minor += U256::from(1),
        |c| c.offer_input_hash = B256::repeat_byte(0x77),
    ];
    for change in changes {
        let mut encrypted = f.encrypted.clone();
        change(&mut encrypted.context);
        assert_unreadable(&f, &encrypted);
    }
    let mut encrypted = f.encrypted.clone();
    encrypted.encrypted_amounts[8] ^= 1;
    assert_unreadable(&f, &encrypted);
}

#[test]
fn wrong_state_and_view_keys_are_rejected() {
    let f = fixture();
    assert!(decrypt_tribute(&[19u8; 32], &f.encrypted).is_err());
    assert!(decrypt_tribute_for_owner(&[19u8; 32], &f.encrypted).is_err());
}

#[test]
fn empty_truncated_and_nonimmutable_blobs_cannot_be_read_as_zero_amounts() {
    let f = fixture();
    let changes: &[fn(&mut EncryptedTributeV2)] = &[
        |t| t.encrypted_amounts.clear(),
        |t| {
            t.encrypted_amounts.pop();
        },
        |t| t.encrypted_amounts[7] = 2,
    ];
    for change in changes {
        let mut encrypted = f.encrypted.clone();
        change(&mut encrypted);
        assert_unreadable(&f, &encrypted);
    }
}

#[test]
fn shared_network_keys_replay_identically_and_serialized_records_contain_only_ciphertext() {
    let f = fixture();
    let restarted = encrypt_tribute(
        &f.tribute_state_key,
        f.encrypted.context.clone(),
        &f.amounts,
    )
    .unwrap();
    assert_eq!(f.encrypted, restarted);
    let wire = postcard::to_allocvec(&f.encrypted).unwrap();
    for plaintext in [
        f.tribute_state_key,
        f.view_key,
        f.amounts.issuance_amount_minor.to_be_bytes::<32>(),
        f.amounts.nominal_amount_minor.to_be_bytes::<32>(),
    ] {
        assert!(!wire.windows(32).any(|bytes| bytes == plaintext));
    }
}

#[test]
fn offer_creator_owns_the_tribute_while_proof_binding_uses_the_transaction_caller() {
    let f = fixture();
    let payload = serde_json::json!({
        "creator": f.encrypted.context.owner,
        "tribute_draft_id": B256::repeat_byte(0x11),
        "amount_base": "100",
        "amount_micro": "0",
        "su_hashes": [B256::repeat_byte(0x22)],
    });
    let cipher_text = outbe_tee::offer_encrypt::encrypt_tribute_offer_with(
        &f.enclave_public,
        [9; 32],
        [1; 12],
        &serde_json::to_vec(&payload).unwrap(),
    )
    .unwrap();
    let caller = Address::repeat_byte(0x99);
    let mut offer = EncryptedTributeOffer {
        owner: caller,
        cipher_text,
        nonce: vec![1; 12],
        ephemeral_pubkey: U256::from_be_bytes(
            PublicKey::from(&StaticSecret::from([9; 32])).to_bytes(),
        ),
        worldwide_day: f.encrypted.context.worldwide_day,
        tribute_currency: 840,
        reference_currency: 978,
        issuance_wwd_vwap_minor: U256::from(2_000_000),
        reference_wwd_vwap_minor: U256::from(2_000_000),
        reference_scurve_minor: U256::ZERO,
        zk_context: Some(TributeZkContext {
            derived_owner: B256::repeat_byte(0x01),
            chain_id: f.encrypted.context.chain_id,
            l2_chain_id: 9_900_501,
        }),
    };
    let key = TributeOfferKeyMaterial {
        tribute_offer_private_key: &f.tribute_state_key,
        salt: &outbe_tee::OFFER_HKDF_SALT,
    };
    let (results, _) = process_encrypted_tribute_offer_batch(
        &key,
        &f.tribute_state_key,
        f.encrypted.context.chain_id,
        &[offer.clone()],
    );
    let result = &results[0];
    assert_eq!(result.status, TributeOfferStatus::Created);
    let tribute = result.tribute.as_ref().unwrap();
    assert_eq!(tribute.context.owner, f.encrypted.context.owner);
    assert_ne!(tribute.context.owner, caller);
    assert_eq!(
        result.token_id,
        outbe_tee_enclave::compute::compute_token_id(
            tribute.context.owner,
            offer.worldwide_day,
            &B256::repeat_byte(0x11).to_string(),
        )
        .unwrap()
    );
    assert_eq!(
        decrypt_tribute(&f.tribute_state_key, tribute).unwrap(),
        f.amounts
    );
    let first_binding = result.zk_expected_hashes.as_ref().unwrap().binding_hash;
    offer.owner = Address::repeat_byte(0x88);
    let (other, _) = process_encrypted_tribute_offer_batch(
        &key,
        &f.tribute_state_key,
        f.encrypted.context.chain_id,
        &[offer],
    );
    assert_eq!(
        other[0].tribute.as_ref().unwrap().context.owner,
        tribute.context.owner
    );
    assert_ne!(
        other[0].zk_expected_hashes.as_ref().unwrap().binding_hash,
        first_binding
    );
}
