//! Tribute-to-NOD bridge checks: rejected terms, a tampered blob, a pure amount
//! round trip, a live resident encrypt and issue, and a signed offer.

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use ed25519_dalek::{Signer, SigningKey};
use outbe_core_common::confidential_tee::{
    decrypt_nod_for_owner, derive_resident_state_key, hkdf_sha256, NOD, TRIBUTE_OFFER_ATTESTATION,
};
use outbe_core_common::nod_encryption::NodTermsV1;
use outbe_core_common::series_id::SeriesId;
use outbe_core_common::tribute_day_encryption::EncryptedTributeDayFractionsV1;
use outbe_core_common::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TributeContextV2,
};
use outbe_core_common::wwd_entity_id::{
    derive_poseidon_digest, derive_poseidon_entity_id, WwdEntityId,
};
use outbe_nod::nod_encryption::decrypt_nod;
use outbe_nod::nod_host::read_nod_amount;
use outbe_system_common::time::{EmissionDate, WorldwideDay};
use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
use outbe_tee::resident::{OfferKeyHandle, StateKeyHandle};
use outbe_tee::TeeAttestationError;
use outbe_tribute::confidential::host::{encrypt_nod_for_tribute, issue_nod_from_tribute};
use outbe_tribute::confidential::{
    derive_tribute_state_key, encrypt_nod_for_tribute_pure, encrypt_tribute,
    issue_nod_from_tribute_pure, seal_day_fractions, tribute_offer_attestation_preimage,
    verify_tribute_offer_attestation, IssueNodFromTributeOutcomeV1, IssueNodFromTributeRequestV1,
    TributeOfferResult, TributeOfferStatus,
};

#[path = "encrypted_tribute/resident.rs"]
mod resident;

const MISMATCH: &str = "offer rejected: NOD source terms mismatch";

#[test]
fn nod_bridge_rejects_mutated_terms_and_a_tampered_tribute_blob() {
    let tribute_key = StateKeyHandle::for_test([7u8; 32]);
    let nod_key = StateKeyHandle::for_test([8u8; 32]);
    let offer = OfferKeyHandle::for_test([6u8; 32]);
    let day = WorldwideDay::new(20_250_115);
    let owner = Address::repeat_byte(3);
    let tribute = encrypt_tribute(
        &tribute_key,
        TributeContextV2 {
            chain_id: 54_322_345,
            tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(4)),
            owner,
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 978,
            tribute_price_minor: U256::ONE,
            offer_input_hash: B256::repeat_byte(5),
        },
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(1_000u64),
            nominal_amount_minor: U256::from(900u64),
        },
    )
    .expect("tribute");
    let terms = NodTermsV1 {
        chain_id: 54_322_345,
        nod_id: outbe_core_common::nod_rules::derive_nod_id(
            SeriesId::for_pair(day.value(), 840, 978).unwrap(),
            owner,
        ),
        owner,
        emission_day: day.value(),
        league_id: 1,
        entry_price_minor: U256::from(700u64),
        issuance_currency: 840,
        reference_currency: 978,
    };
    let fractions = EncryptedTributeDayFractionsV1 {
        chain_id: terms.chain_id,
        worldwide_day: day,
        emission_day: EmissionDate::new(terms.emission_day),
        completed_at: 1,
        operation_hash: B256::ZERO,
        encrypted_fractions: Vec::new(),
    };
    for wrong in [
        mutated(&terms, |terms| terms.owner = Address::repeat_byte(9)),
        mutated(&terms, |terms| terms.emission_day = 20_250_116),
        mutated(&terms, |terms| terms.issuance_currency = 826),
        mutated(&terms, |terms| terms.reference_currency = 826),
    ] {
        let encrypted = encrypt_nod_for_tribute_pure(
            &tribute_key,
            &nod_key,
            &tribute,
            wrong.clone(),
            U256::from(123u64),
        )
        .expect_err("encrypt rejects mutated terms");
        assert_eq!(encrypted.to_string(), MISMATCH);
        let issued = issue_nod_from_tribute_pure(
            &offer,
            &tribute_key,
            &nod_key,
            &IssueNodFromTributeRequestV1 {
                tribute: tribute.clone(),
                day_fractions: fractions.clone(),
                terms: wrong,
            },
        )
        .expect_err("issue rejects mutated terms");
        assert_eq!(issued.to_string(), MISMATCH);
    }

    for index in [16usize, tribute.encrypted_amounts.len() - 1] {
        let mut tampered = tribute.clone();
        tampered.encrypted_amounts[index] ^= 1;
        let encrypted = encrypt_nod_for_tribute_pure(
            &tribute_key,
            &nod_key,
            &tampered,
            terms.clone(),
            U256::from(123u64),
        )
        .expect_err("encrypt rejects a tampered blob");
        assert_ne!(encrypted.to_string(), MISMATCH, "byte {index}");
        let issued = issue_nod_from_tribute_pure(
            &offer,
            &tribute_key,
            &nod_key,
            &IssueNodFromTributeRequestV1 {
                tribute: tampered,
                day_fractions: fractions.clone(),
                terms: terms.clone(),
            },
        )
        .expect_err("issue rejects a tampered blob");
        assert_ne!(issued.to_string(), MISMATCH, "byte {index}");
    }
}

#[test]
fn pure_bridge_repeats_amount_123_and_rejects_legacy_nod_ids() {
    let tribute_key = StateKeyHandle::for_test([7u8; 32]);
    let nod_key = StateKeyHandle::for_test([8u8; 32]);
    let (tribute, terms) = bridge_fixture(&tribute_key);
    let day = WorldwideDay::new(terms.emission_day);
    let owner = terms.owner;
    let view_key = hkdf_sha256(&[8u8; 32], owner.as_slice(), b"outbe/nod/view-key/v1")
        .expect("owner view key");
    for invalid_id in [
        U256::from_be_bytes(derive_poseidon_digest(owner, day).0),
        derive_poseidon_entity_id(owner, day).to_u256(),
        outbe_core_common::nod_rules::derive_nod_id(
            SeriesId::for_pair(day.value(), 840, 978).unwrap(),
            Address::repeat_byte(9),
        ),
        outbe_core_common::nod_rules::derive_nod_id(
            SeriesId::for_pair(20_250_116, 840, 978).unwrap(),
            owner,
        ),
    ] {
        let mut invalid = terms.clone();
        invalid.nod_id = invalid_id;
        let rejected = encrypt_nod_for_tribute_pure(
            &tribute_key,
            &nod_key,
            &tribute,
            invalid,
            U256::from(123u64),
        )
        .expect_err("nod_id-only substitution");
        assert_eq!(rejected.to_string(), MISMATCH);
    }
    let first = encrypt_nod_for_tribute_pure(
        &tribute_key,
        &nod_key,
        &tribute,
        terms.clone(),
        U256::from(123u64),
    )
    .expect("amount 123");
    let repeated = encrypt_nod_for_tribute_pure(
        &tribute_key,
        &nod_key,
        &tribute,
        terms.clone(),
        U256::from(123u64),
    )
    .expect("repeated amount 123");
    let divergent = encrypt_nod_for_tribute_pure(
        &tribute_key,
        &nod_key,
        &tribute,
        terms.clone(),
        U256::from(124u64),
    )
    .expect("amount 124");
    assert_eq!(first, repeated);
    assert_ne!(first.encryption_binding, divergent.encryption_binding);
    assert_ne!(
        first.encrypted_gratis_amount,
        divergent.encrypted_gratis_amount
    );
    let mut corrupted = tribute;
    corrupted.encrypted_amounts[16] ^= 1;
    assert!(encrypt_nod_for_tribute_pure(
        &tribute_key,
        &nod_key,
        &corrupted,
        terms,
        U256::from(123u64),
    )
    .is_err());
    assert_eq!(decrypt_nod(&nod_key, &first).unwrap(), U256::from(123u64));
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &first).unwrap(),
        U256::from(123u64)
    );
    assert!(decrypt_nod_for_owner(&[12u8; 32], &first).is_err());
    assert!(decrypt_nod(&StateKeyHandle::for_test([9u8; 32]), &first).is_err());
    let mut tampered = first.clone();
    tampered.terms.owner = Address::repeat_byte(9);
    assert!(decrypt_nod(&nod_key, &tampered).is_err());
    let mut tampered = first;
    tampered.encrypted_gratis_amount[10] ^= 1;
    assert!(decrypt_nod_for_owner(&view_key, &tampered).is_err());
}

#[test]
fn resident_host_encrypts_reads_and_issues_nod() {
    let context = Arc::new(resident::context(54_322_345));
    let tribute_key = resident::state(&context, derive_tribute_state_key);
    let nod_key = derive_resident_state_key(&context, &NOD, 0).expect("nod state key");
    let word = [B256::repeat_byte(1).0];
    let label = b"outbe/nod/domain-separation/v1";
    assert_ne!(
        tribute_key.compute_domain_mac(label, &word).unwrap(),
        nod_key.compute_domain_mac(label, &word).unwrap()
    );
    let (tribute, terms) = bridge_fixture(&tribute_key);
    let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&context)));
    let first = encrypt_nod_for_tribute(tribute.clone(), terms.clone(), U256::from(123u64))
        .expect("live encrypt");
    let repeated = encrypt_nod_for_tribute(tribute.clone(), terms.clone(), U256::from(123u64))
        .expect("live repeat");
    assert_eq!(first, repeated);
    assert_eq!(read_nod_amount(&first).unwrap(), U256::from(123u64));
    let mut mutated = first;
    mutated.terms.owner = Address::repeat_byte(9);
    assert!(read_nod_amount(&mutated).is_err());

    let mut fractions = BTreeMap::new();
    fractions.insert(1u16, 1_000_000u64);
    let day = tribute.context.worldwide_day;
    let sealed = context
        .with_offer_key_handle(|offer| {
            seal_day_fractions(
                offer,
                (
                    terms.chain_id,
                    day,
                    EmissionDate::new(terms.emission_day),
                    1,
                ),
                B256::ZERO,
                &fractions,
            )
        })
        .expect("offer key")
        .expect("sealed fractions");
    let issued = issue_nod_from_tribute(IssueNodFromTributeRequestV1 {
        tribute,
        day_fractions: sealed,
        terms,
    })
    .expect("live issue");
    let IssueNodFromTributeOutcomeV1::Issued(nod) = issued else {
        panic!("live issue did not return Issued");
    };
    assert_eq!(read_nod_amount(&nod).unwrap(), U256::from(900u64));
}

#[test]
fn tribute_offer_attestation_rejects_a_mutated_price_and_a_one_byte_tag() {
    let results = vec![TributeOfferResult {
        token_id: B256::repeat_byte(0x11),
        owner: Address::repeat_byte(0x22),
        issuance_amount_minor: U256::from(1_000u64),
        nominal_amount_minor: U256::from(2_000u64),
        effective_reference_price_minor: U256::from(3_000u64),
        su_hashes: vec!["0xabc".to_string()],
        wallet_addresses: Vec::new(),
        sra_addresses: Vec::new(),
        zk_expected_hashes: None,
        status: TributeOfferStatus::Created,
    }];
    let hash = B256::repeat_byte(0xAB);
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let public_key = signing_key.verifying_key().to_bytes();
    let preimage = tribute_offer_attestation_preimage(hash, &results).expect("preimage");
    let tag = signing_key.sign(&preimage).to_bytes();
    verify_tribute_offer_attestation(&public_key, hash, &results, &tag).expect("valid tag");

    let mut tampered = results.clone();
    tampered[0].effective_reference_price_minor += U256::ONE;
    let mismatch =
        verify_tribute_offer_attestation(&public_key, hash, &tampered, &tag).expect_err("price");
    assert!(matches!(
        &mismatch,
        outbe_tee::TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::SignatureMismatch(_),
        } if *descriptor == TRIBUTE_OFFER_ATTESTATION
    ));
    let short =
        verify_tribute_offer_attestation(&public_key, hash, &results, &[0u8; 1]).expect_err("tag");
    assert!(matches!(
        &short,
        outbe_tee::TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::InvalidSignatureLength { actual: 1 },
        } if *descriptor == TRIBUTE_OFFER_ATTESTATION
    ));
    let other_hash = B256::repeat_byte(0xAC);
    let wrong_inputs = verify_tribute_offer_attestation(&public_key, other_hash, &results, &tag)
        .expect_err("inputs hash");
    assert!(matches!(
        &wrong_inputs,
        outbe_tee::TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::SignatureMismatch(_),
        } if *descriptor == TRIBUTE_OFFER_ATTESTATION
    ));
}

fn bridge_fixture(tribute_key: &StateKeyHandle) -> (EncryptedTributeV2, NodTermsV1) {
    let day = WorldwideDay::new(20_250_115);
    let owner = Address::repeat_byte(3);
    let tribute = encrypt_tribute(
        tribute_key,
        TributeContextV2 {
            chain_id: 54_322_345,
            tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(4)),
            owner,
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 978,
            tribute_price_minor: U256::ONE,
            offer_input_hash: B256::repeat_byte(5),
        },
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(1_000u64),
            nominal_amount_minor: U256::from(900u64),
        },
    )
    .expect("tribute");
    let terms = NodTermsV1 {
        chain_id: 54_322_345,
        nod_id: outbe_core_common::nod_rules::derive_nod_id(
            SeriesId::for_pair(day.value(), 840, 978).unwrap(),
            owner,
        ),
        owner,
        emission_day: day.value(),
        league_id: 0,
        entry_price_minor: U256::from(700u64),
        issuance_currency: 840,
        reference_currency: 978,
    };
    (tribute, terms)
}

fn mutated(terms: &NodTermsV1, edit: impl FnOnce(&mut NodTermsV1)) -> NodTermsV1 {
    let mut terms = terms.clone();
    edit(&mut terms);
    // Leave nod_id on the original series. A recomputed id for a new emission day
    // still matches terms_match, which does not compare that day.
    terms
}
