//! Owner exports bind the ledger, account and ephemeral recipient before resident authorization.
use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::{
    decrypt_share, derive_account_keys_message, domain_for, eip191_hash, BalanceTransition,
    EncryptedShare, Ledger, GRATIS_BALANCE_ENVELOPE,
};
use outbe_tee::resident::AccountKeysHandle;

#[test]
fn export_authorizes_only_the_exact_domain_account_and_recipient() {
    let signer = k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
    let owner = Address::from_public_key(signer.verifying_key());
    let recipient =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([8; 32])).to_bytes();
    let keys = AccountKeysHandle::for_test(owner, [9; 32], [10; 32]);
    let digest = eip191_hash(&derive_account_keys_message(
        Ledger::Gratis,
        owner,
        B256::from(recipient),
    ));
    let (sig, recovery) = signer.sign_prehash_recoverable(digest.as_slice()).unwrap();
    let mut signature = [0; 65];
    signature[..64].copy_from_slice(&sig.to_bytes());
    signature[64] = recovery.to_byte();
    let sealed = domain_for(Ledger::Gratis)
        .export_sealed_account_keys(&keys, &signature, &recipient)
        .unwrap();
    let opened = decrypt_share(
        &[8; 32],
        &EncryptedShare {
            ephemeral_pub: sealed.enclave_ephemeral_pubkey,
            nonce: sealed.nonce,
            ciphertext: sealed.sealed,
        },
    )
    .unwrap();
    assert_eq!(opened, [[9; 32], [10; 32]].concat());
    for ledger in [Ledger::Promis, Ledger::Nod] {
        assert!(domain_for(ledger)
            .export_sealed_account_keys(&keys, &signature, &recipient)
            .is_err());
    }
    let other = AccountKeysHandle::for_test(Address::repeat_byte(1), [9; 32], [10; 32]);
    assert!(domain_for(Ledger::Gratis)
        .export_sealed_account_keys(&other, &signature, &recipient)
        .is_err());
    assert!(domain_for(Ledger::Gratis)
        .export_sealed_account_keys(&keys, &signature, &[0; 32])
        .is_err());
}

#[test]
fn resident_failures_preserve_domain_rejection_messages_and_typed_causes() {
    use outbe_core_common::confidential_tee::ecdhe_offer_decrypt;
    use outbe_tee::resident::{OfferKeyHandle, StateKeyHandle};
    use std::error::Error as _;
    let error = ecdhe_offer_decrypt(
        &OfferKeyHandle::for_test([7; 32]),
        &[0; 32],
        &[8; 32],
        &[0; 12],
        &[0; 16],
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "AEAD decryption failed (bad key, nonce, or tampered ciphertext)"
    );
    assert!(error.source().is_some());
    let error = GRATIS_BALANCE_ENVELOPE
        .write_amount(
            &StateKeyHandle::for_test([1; 32]),
            BalanceTransition {
                account: Address::repeat_byte(2),
                field: 0,
                previous_version: u64::MAX,
                amount: alloy_primitives::U256::from(1),
                input_hash: B256::ZERO,
            },
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "AEAD encryption failed");
}
