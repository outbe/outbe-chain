use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    nod_encryption::NodTermsV2,
    time::WorldwideDay,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};
use outbe_tee::nod_decrypt::decrypt_nod_for_owner;
use outbe_tee_enclave::{
    nod_encryption::{decrypt_nod, encrypt_nod_for_tribute},
    tribute_encryption::encrypt_tribute,
};
use x25519_dalek::{PublicKey, StaticSecret};
#[test]
fn self_contained_nod_rederives_keys_and_separates_divergent_amounts() {
    let network = [7u8; 32];
    let owner = [11u8; 32];
    let public = PublicKey::from(&StaticSecret::from(owner)).to_bytes();
    let network_public = PublicKey::from(&StaticSecret::from(network)).to_bytes();
    let day = WorldwideDay::new(20250115);
    let owner_address = Address::repeat_byte(3);
    let tribute = encrypt_tribute(
        &network,
        &public,
        TributeContextV2 {
            chain_id: 54322345,
            tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(4)),
            owner: owner_address,
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 978,
            tribute_price_minor: U256::ONE,
            exclude_from_intex_issuance: false,
            offer_input_hash: B256::repeat_byte(5),
        },
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(1000),
            nominal_amount_minor: U256::from(900),
        },
    )
    .unwrap();
    let terms = NodTermsV2 {
        chain_id: 54322345,
        nod_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(6)),
        owner: owner_address,
        worldwide_day: day,
        league_id: 0,
        entry_price_minor: U256::from(700),
        issuance_currency: 840,
        reference_currency: 978,
    };
    let first =
        encrypt_nod_for_tribute(&network, &tribute, terms.clone(), U256::from(123)).unwrap();
    let repeated =
        encrypt_nod_for_tribute(&network, &tribute, terms.clone(), U256::from(123)).unwrap();
    let divergent = encrypt_nod_for_tribute(&network, &tribute, terms, U256::from(124)).unwrap();
    assert_eq!(first, repeated);
    assert_ne!(first.encryption_binding, divergent.encryption_binding);
    assert_ne!(
        first.encrypted_gratis_amount,
        divergent.encrypted_gratis_amount
    );
    drop(tribute);
    assert_eq!(decrypt_nod(&network, &first).unwrap(), U256::from(123));
    assert_eq!(decrypt_nod(&network, &first).unwrap(), U256::from(123));
    assert_eq!(
        decrypt_nod_for_owner(&owner, &network_public, &first).unwrap(),
        U256::from(123)
    );
    assert!(decrypt_nod_for_owner(&[12u8; 32], &network_public, &first).is_err());
    assert!(decrypt_nod(&[8u8; 32], &first).is_err());
    let mut tampered = first.clone();
    tampered.terms.owner = Address::repeat_byte(9);
    assert!(decrypt_nod(&network, &tampered).is_err());
    let mut tampered = first;
    tampered.encrypted_creator_public_key[10] ^= 1;
    assert!(decrypt_nod_for_owner(&owner, &network_public, &tampered).is_err());
}
