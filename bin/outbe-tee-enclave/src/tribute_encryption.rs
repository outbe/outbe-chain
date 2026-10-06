//! Enclave-resident Tribute encryption with repeated ECDH on every read.

use outbe_primitives::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TributeContextV2, TRIBUTE_AMOUNT_NONCE_INFO,
};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::{
    confidential::Domain,
    crypto::hkdf_sha256,
    errors::{Result, TeeError},
};

const CREATOR_PUBLIC_KEY: Domain = Domain {
    state_info: b"outbe/tribute/creator-key/state/v2",
    view_info: b"outbe/tribute/creator-key/view/v2",
    modify_info: b"outbe/tribute/creator-key/modify/v2",
    nonce_info: b"outbe/tribute/creator-key/nonce/v2",
    modify_tag: b"outbe/tribute/creator-key/auth/v2",
};

const AMOUNTS: Domain = Domain {
    state_info: b"outbe/tribute/amount/state/v2",
    view_info: b"outbe/tribute/amount/view/v2",
    modify_info: b"outbe/tribute/amount/modify/v2",
    nonce_info: TRIBUTE_AMOUNT_NONCE_INFO,
    modify_tag: b"outbe/tribute/amount/auth/v2",
};

/// Encrypt the validated, deterministically calculated amounts of one offer.
/// The caller must bind context.offer_input_hash to that exact encrypted offer.
pub fn encrypt_tribute(
    network_secret: &[u8; 32],
    creator_public: &[u8; 32],
    context: TributeContextV2,
    amounts: &TributeAmountsV2,
) -> Result<EncryptedTributeV2> {
    let slot = context.crypto_slot();
    let encrypted_creator_public_key =
        CREATOR_PUBLIC_KEY.write_blob(network_secret, slot, 0, 0, creator_public)?;
    let amount_key = derive_amount_key(
        network_secret,
        creator_public,
        &context,
        &encrypted_creator_public_key,
    )?;
    let plaintext = Zeroizing::new(amounts.to_be_bytes());
    let encrypted_amounts = AMOUNTS.write_blob(&amount_key, slot, 0, 0, plaintext.as_ref())?;
    Ok(EncryptedTributeV2 {
        context,
        encrypted_creator_public_key,
        encrypted_amounts,
    })
}

/// Private read bridge. The creator key and the amount key are not retained.
pub fn decrypt_tribute(
    network_secret: &[u8; 32],
    tribute: &EncryptedTributeV2,
) -> Result<TributeAmountsV2> {
    if !tribute.has_valid_encoding() {
        return Err(TeeError::DecryptFailed);
    }
    let slot = tribute.context.crypto_slot();
    let (_, creator_bytes) = CREATOR_PUBLIC_KEY.read_blob(
        network_secret,
        slot,
        0,
        &tribute.encrypted_creator_public_key,
    )?;
    let creator_public = creator_bytes
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    let key = derive_amount_key(
        network_secret,
        creator_public,
        &tribute.context,
        &tribute.encrypted_creator_public_key,
    )?;
    let (_, plaintext) = AMOUNTS.read_blob(&key, slot, 0, &tribute.encrypted_amounts)?;
    let plaintext = Zeroizing::new(plaintext);
    let amounts = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    Ok(TributeAmountsV2::from_be_bytes(amounts))
}

fn derive_amount_key(
    network_secret: &[u8; 32],
    creator_public: &[u8; 32],
    context: &TributeContextV2,
    encrypted_creator_public_key: &[u8],
) -> Result<Zeroizing<[u8; 32]>> {
    let secret = StaticSecret::from(*network_secret);
    let shared = secret.diffie_hellman(&PublicKey::from(*creator_public));
    if !shared.was_contributory() {
        return Err(TeeError::InvalidKeyLen);
    }
    let enclave_public = PublicKey::from(&secret).to_bytes();
    let info = context.amount_key_info(
        &enclave_public,
        creator_public,
        encrypted_creator_public_key,
    );
    let key = hkdf_sha256(context.digest().as_slice(), shared.as_bytes(), &info)?;
    Ok(Zeroizing::new(key))
}
