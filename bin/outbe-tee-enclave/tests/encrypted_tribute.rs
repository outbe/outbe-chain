use alloy_primitives::{Address, B256, U256};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TributeContextV2,
};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_tee::tribute_decrypt::decrypt_tribute_for_creator;
use outbe_tee_enclave::tribute_encryption::{decrypt_tribute, encrypt_tribute};
use x25519_dalek::{PublicKey, StaticSecret};

struct Fixture {
    enclave_secret: [u8; 32],
    creator_secret: [u8; 32],
    enclave_public: [u8; 32],
    encrypted: EncryptedTributeV2,
    amounts: TributeAmountsV2,
}

fn fixture() -> Fixture {
    let enclave_secret = [7u8; 32];
    let creator_secret = [11u8; 32];
    let enclave_public = PublicKey::from(&StaticSecret::from(enclave_secret)).to_bytes();
    let creator_public = PublicKey::from(&StaticSecret::from(creator_secret)).to_bytes();
    let day = WorldwideDay::new(20250115);
    let context = TributeContextV2 {
        chain_id: 54322345,
        tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(0x22)),
        owner: Address::repeat_byte(0x33),
        worldwide_day: day,
        issuance_currency: 840,
        reference_currency: 978,
        tribute_price_minor: U256::from(2_000_000),
        exclude_from_intex_issuance: false,
        offer_input_hash: B256::repeat_byte(0x44),
    };
    let amounts = TributeAmountsV2 {
        issuance_amount_minor: U256::from(100_000_000),
        nominal_amount_minor: U256::from(50_000_000),
    };
    let encrypted = encrypt_tribute(&enclave_secret, &creator_public, context, &amounts).unwrap();
    Fixture {
        enclave_secret,
        creator_secret,
        enclave_public,
        encrypted,
        amounts,
    }
}

#[test]
fn creator_and_enclave_can_read_the_same_encrypted_tribute_without_a_saved_amount_key() {
    let Fixture {
        enclave_secret,
        creator_secret,
        enclave_public,
        encrypted,
        amounts,
    } = fixture();

    assert_eq!(encrypted.encrypted_creator_public_key.len(), 56);
    assert_eq!(encrypted.encrypted_amounts.len(), 88);
    assert_eq!(
        decrypt_tribute(&enclave_secret, &encrypted).unwrap(),
        amounts
    );
    assert_eq!(
        decrypt_tribute_for_creator(&creator_secret, &enclave_public, &encrypted).unwrap(),
        amounts
    );
    assert_eq!(
        decrypt_tribute(&enclave_secret, &encrypted).unwrap(),
        amounts
    );
}

#[test]
fn both_readers_reject_a_corrupted_encrypted_creator_key() {
    let f = fixture();
    let mut corrupted = f.encrypted.clone();
    corrupted.encrypted_creator_public_key[8] ^= 1;
    assert!(decrypt_tribute(&f.enclave_secret, &corrupted).is_err());
    assert!(decrypt_tribute_for_creator(&f.creator_secret, &f.enclave_public, &corrupted).is_err());
}

fn assert_unreadable(f: &Fixture, encrypted: &EncryptedTributeV2) {
    assert!(decrypt_tribute(&f.enclave_secret, encrypted).is_err());
    assert!(decrypt_tribute_for_creator(&f.creator_secret, &f.enclave_public, encrypted).is_err());
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
        |c| c.exclude_from_intex_issuance = true,
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
fn wrong_private_keys_and_noncontributory_public_keys_are_rejected() {
    let f = fixture();
    assert!(decrypt_tribute(&[19u8; 32], &f.encrypted).is_err());
    assert!(decrypt_tribute_for_creator(&[19u8; 32], &f.enclave_public, &f.encrypted).is_err());
    assert!(decrypt_tribute_for_creator(&f.creator_secret, &[0; 32], &f.encrypted).is_err());
    assert!(encrypt_tribute(&f.enclave_secret, &[0; 32], f.encrypted.context, &f.amounts).is_err());
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
        |t| t.encrypted_creator_public_key.clear(),
        |t| {
            t.encrypted_creator_public_key.pop();
        },
        |t| t.encrypted_creator_public_key[7] = 2,
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
    let creator_public = PublicKey::from(&StaticSecret::from(f.creator_secret)).to_bytes();
    let restarted = encrypt_tribute(
        &f.enclave_secret,
        &creator_public,
        f.encrypted.context.clone(),
        &f.amounts,
    )
    .unwrap();
    assert_eq!(f.encrypted, restarted);
    let wire = postcard::to_allocvec(&f.encrypted).unwrap();
    for plaintext in [
        creator_public,
        f.enclave_secret,
        f.creator_secret,
        f.amounts.issuance_amount_minor.to_be_bytes::<32>(),
        f.amounts.nominal_amount_minor.to_be_bytes::<32>(),
    ] {
        assert!(!wire.windows(32).any(|bytes| bytes == plaintext));
    }
}
