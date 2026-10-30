use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    nod_encryption::NodTermsV1,
    time::WorldwideDay,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};
use outbe_tee::nod_decrypt::decrypt_nod_for_owner;
use outbe_tee_enclave::{
    nod_encryption::{decrypt_nod, derive_view_key, encrypt_nod_for_tribute},
    tribute_encryption::encrypt_tribute,
};
#[test]
fn self_contained_nod_rederives_keys_and_separates_divergent_amounts() {
    let tribute_key = [7u8; 32];
    let nod_key = [8u8; 32];
    let day = WorldwideDay::new(20250115);
    let owner_address = Address::repeat_byte(3);
    let nod_view_key = derive_view_key(&nod_key, owner_address).unwrap();
    let tribute = encrypt_tribute(
        &tribute_key,
        TributeContextV2 {
            chain_id: 54322345,
            tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(4)),
            owner: owner_address,
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 978,
            tribute_price_minor: U256::ONE,
            offer_input_hash: B256::repeat_byte(5),
        },
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(1000),
            nominal_amount_minor: U256::from(900),
        },
    )
    .unwrap();
    let terms = NodTermsV1 {
        chain_id: 54322345,
        nod_id: outbe_primitives::nod_rules::derive_nod_id(
            outbe_primitives::series_id::SeriesId::for_pair(day.value(), 840, 978).unwrap(),
            owner_address,
        ),
        owner: owner_address,
        worldwide_day: day,
        emission_day: day.value(),
        league_id: 0,
        entry_price_minor: U256::from(700),
        issuance_currency: 840,
        reference_currency: 978,
    };
    for invalid_id in [
        U256::from_be_bytes(
            outbe_primitives::wwd_entity_id::derive_poseidon_digest(owner_address, day).0,
        ),
        outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(owner_address, day).to_u256(),
        outbe_primitives::nod_rules::derive_nod_id(
            outbe_primitives::series_id::SeriesId::for_pair(day.value(), 840, 978).unwrap(),
            Address::repeat_byte(9),
        ),
        outbe_primitives::nod_rules::derive_nod_id(
            outbe_primitives::series_id::SeriesId::for_pair(20250116, 840, 978).unwrap(),
            owner_address,
        ),
    ] {
        let mut invalid = terms.clone();
        invalid.nod_id = invalid_id;
        assert!(encrypt_nod_for_tribute(
            &tribute_key,
            &nod_key,
            &tribute,
            invalid,
            U256::from(123)
        )
        .is_err());
    }
    let first = encrypt_nod_for_tribute(
        &tribute_key,
        &nod_key,
        &tribute,
        terms.clone(),
        U256::from(123),
    )
    .unwrap();
    let repeated = encrypt_nod_for_tribute(
        &tribute_key,
        &nod_key,
        &tribute,
        terms.clone(),
        U256::from(123),
    )
    .unwrap();
    let divergent = encrypt_nod_for_tribute(
        &tribute_key,
        &nod_key,
        &tribute,
        terms.clone(),
        U256::from(124),
    )
    .unwrap();
    assert_eq!(first, repeated);
    assert_ne!(first.encryption_binding, divergent.encryption_binding);
    assert_ne!(
        first.encrypted_gratis_amount,
        divergent.encrypted_gratis_amount
    );
    let mut corrupted_tribute = tribute;
    corrupted_tribute.encrypted_amounts[16] ^= 1;
    assert!(encrypt_nod_for_tribute(
        &tribute_key,
        &nod_key,
        &corrupted_tribute,
        terms,
        U256::from(123),
    )
    .is_err());
    assert_eq!(decrypt_nod(&nod_key, &first).unwrap(), U256::from(123));
    assert_eq!(decrypt_nod(&nod_key, &first).unwrap(), U256::from(123));
    assert_eq!(
        decrypt_nod_for_owner(&nod_view_key, &first).unwrap(),
        U256::from(123)
    );
    assert!(decrypt_nod_for_owner(&[12u8; 32], &first).is_err());
    assert!(decrypt_nod(&[9u8; 32], &first).is_err());
    let mut tampered = first.clone();
    tampered.terms.owner = Address::repeat_byte(9);
    assert!(decrypt_nod(&nod_key, &tampered).is_err());
    let mut tampered = first;
    tampered.encrypted_gratis_amount[10] ^= 1;
    assert!(decrypt_nod_for_owner(&nod_view_key, &tampered).is_err());
}
