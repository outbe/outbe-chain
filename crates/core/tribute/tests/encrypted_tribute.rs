use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{
    decrypt_tribute_for_owner, encrypt_tribute_offer_with, hkdf_sha256, OFFER_HKDF_SALT,
};
use outbe_core_common::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TributeContextV2,
};
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_system_common::time::WorldwideDay;
use outbe_tee::resident::OfferKeyHandle;
use outbe_tribute::confidential::{
    compute_token_id, decrypt_tribute, encrypt_tribute, process_encrypted_tribute_offer_batch,
    EncryptedTributeOffer, TributeOfferKeyMaterial, TributeOfferStatus, TributeZkContext,
};
use x25519_dalek::{PublicKey, StaticSecret};

struct Fixture {
    tribute_state_key: OfferKeyHandle,
    view_key: [u8; 32],
    enclave_public: [u8; 32],
    encrypted: EncryptedTributeV2,
    amounts: TributeAmountsV2,
}

fn fixture() -> Fixture {
    let tribute_state_key = OfferKeyHandle::for_test([7u8; 32]);
    let enclave_public = tribute_state_key.public();
    let day = WorldwideDay::new(20250115);
    let owner = Address::repeat_byte(0x33);
    let view_key = hkdf_sha256(&[7; 32], owner.as_slice(), b"outbe/tribute/view-key/v1").unwrap();
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
    let encrypted = encrypt_tribute(tribute_state_key.key_handle(), context, &amounts).unwrap();
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
        decrypt_tribute(tribute_state_key.key_handle(), &encrypted).unwrap(),
        amounts
    );
    assert_eq!(
        decrypt_tribute_for_owner(&view_key, &encrypted).unwrap(),
        amounts
    );
    assert_eq!(
        decrypt_tribute(tribute_state_key.key_handle(), &encrypted).unwrap(),
        amounts
    );
}

fn assert_unreadable(f: &Fixture, encrypted: &EncryptedTributeV2) {
    assert!(decrypt_tribute(f.tribute_state_key.key_handle(), encrypted).is_err());
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
    assert!(decrypt_tribute(
        OfferKeyHandle::for_test([19u8; 32]).key_handle(),
        &f.encrypted
    )
    .is_err());
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
        f.tribute_state_key.key_handle(),
        f.encrypted.context.clone(),
        &f.amounts,
    )
    .unwrap();
    assert_eq!(f.encrypted, restarted);
    let wire = postcard::to_allocvec(&f.encrypted).unwrap();
    for plaintext in [
        [7; 32],
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
    let cipher_text = encrypt_tribute_offer_with(
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
        salt: &OFFER_HKDF_SALT,
    };
    let (results, _) = process_encrypted_tribute_offer_batch(
        &key,
        f.tribute_state_key.key_handle(),
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
        compute_token_id(
            tribute.context.owner,
            offer.worldwide_day,
            &B256::repeat_byte(0x11).to_string(),
        )
        .unwrap()
    );
    assert_eq!(
        decrypt_tribute(f.tribute_state_key.key_handle(), tribute).unwrap(),
        f.amounts
    );
    let first_binding = result.zk_expected_hashes.as_ref().unwrap().binding_hash;
    offer.owner = Address::repeat_byte(0x88);
    let (other, _) = process_encrypted_tribute_offer_batch(
        &key,
        f.tribute_state_key.key_handle(),
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

#[path = "encrypted_tribute/resident.rs"]
mod resident;

#[test]
fn resident_dispatch_reads_tribute_and_rejects_tampering() {
    use std::sync::{Arc, Mutex};

    use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
    use outbe_tee::witness::{
        EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use outbe_tribute::confidential::host::read_tribute_amounts;
    use outbe_tribute::confidential::{
        derive_tribute_state_key, tribute_read_attestation_preimage,
    };

    let context = Arc::new(resident::context(54322345));
    let key = resident::state(&context, derive_tribute_state_key);
    let f = fixture();
    let encrypted = encrypt_tribute(&key, f.encrypted.context.clone(), &f.amounts).unwrap();
    let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&context)));
    let leader = context.attestation_public();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let scope = WitnessExecutionScope::enter(Arc::clone(&state));
    let amounts = read_tribute_amounts(vec![encrypted.clone()]).unwrap();
    assert_eq!(amounts, vec![f.amounts]);
    let staged = state.lock().unwrap().staged.clone();
    let EnclaveWitnessEntryV1::Payload {
        inputs_canonical_hash,
        attestation_tag,
        ..
    } = &staged[0]
    else {
        panic!("recorded entry is not Payload");
    };
    let preimage = tribute_read_attestation_preimage(*inputs_canonical_hash, &amounts).unwrap();
    let public = ed25519_dalek::VerifyingKey::from_bytes(&leader).unwrap();
    public
        .verify_strict(
            &preimage,
            &ed25519_dalek::Signature::from_slice(attestation_tag).unwrap(),
        )
        .unwrap();
    drop(scope);
    let mut corrupted = encrypted;
    corrupted.encrypted_amounts[8] ^= 1;
    assert!(read_tribute_amounts(vec![corrupted]).is_err());
}
