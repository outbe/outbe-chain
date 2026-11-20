//! Primitive operations keep derived keys and plaintext working buffers zeroizing.
use super::{AccountKeysHandle, Result, TeeCryptoError};
use alloy_primitives::B256;
use ring::{aead, hmac, rand::SecureRandom as _};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

pub(crate) fn derive(salt: &[u8], ikm: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    Ok(Zeroizing::new(crate::tee_encrypt::hkdf_sha256(
        salt, ikm, info,
    )?))
}
pub(crate) fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(*secret)).to_bytes()
}
pub(crate) fn shared(secret: &[u8; 32], peer: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>> {
    let shared = StaticSecret::from(*secret).diffie_hellman(&PublicKey::from(*peer));
    if !shared.was_contributory() {
        return Err(TeeCryptoError::InvalidContext);
    }
    Ok(Zeroizing::new(*shared.as_bytes()))
}
/// Preserve the original offer/DKG ECDH recipe, including low-order peer behavior.
pub(crate) fn shared_legacy(secret: &[u8; 32], peer: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let shared = StaticSecret::from(*secret).diffie_hellman(&PublicKey::from(*peer));
    Zeroizing::new(*shared.as_bytes())
}
pub(crate) fn nonce(key: &[u8; 32], context: &[u8], info: &[u8]) -> Result<[u8; 12]> {
    let bytes = derive(key, context, info)?;
    let mut nonce = [0; 12];
    nonce.copy_from_slice(&bytes[..12]);
    Ok(nonce)
}
pub(crate) fn mac(key: &[u8; 32], label: &[u8], message: &[u8]) -> [u8; 32] {
    let mut context = Zeroizing::new(label.to_vec());
    context.extend_from_slice(message);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &context);
    let mut out = [0; 32];
    out.copy_from_slice(tag.as_ref());
    out
}
pub(crate) fn verify_mac(key: &[u8; 32], label: &[u8], message: &[u8], tag: &[u8; 32]) -> bool {
    let mut context = Zeroizing::new(label.to_vec());
    context.extend_from_slice(message);
    hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, key), &context, tag).is_ok()
}
pub(crate) fn seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Result<Vec<u8>> {
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
            .map_err(crate::errors::TeeCryptoError::aead_key)?,
    );
    let mut out = Zeroizing::new(plaintext.to_vec());
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(*nonce),
        aead::Aad::empty(),
        &mut *out,
    )
    .map_err(crate::errors::TeeCryptoError::aead_seal)?;
    Ok(std::mem::take(&mut *out))
}
pub(crate) fn open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
            .map_err(crate::errors::TeeCryptoError::aead_key)?,
    );
    let mut out = Zeroizing::new(ciphertext.to_vec());
    let len = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(*nonce),
            aead::Aad::empty(),
            &mut out,
        )
        .map_err(|_| crate::errors::TeeCryptoError::DecryptionFailed)?
        .len();
    out.truncate(len);
    Ok(out)
}
pub(crate) fn random<const N: usize>() -> Result<Zeroizing<[u8; N]>> {
    let mut bytes = Zeroizing::new([0; N]);
    ring::rand::SystemRandom::new()
        .fill(bytes.as_mut())
        .map_err(crate::errors::TeeCryptoError::rng_failure)?;
    Ok(bytes)
}

#[derive(Debug, PartialEq, Eq)]
pub struct SealedAccountKeysExport {
    pub sealed: Vec<u8>,
    pub nonce: [u8; 12],
    pub enclave_ephemeral_pubkey: [u8; 32],
}

pub fn export_sealed_account_keys(
    handle: &AccountKeysHandle,
    preimage_hash: B256,
    owner_signature: &[u8; 65],
    requester_ephemeral_x25519: &[u8; 32],
    hkdf_info: &[u8],
) -> Result<SealedAccountKeysExport> {
    export_sealed_account_keys_inner(
        handle,
        (preimage_hash, owner_signature),
        requester_ephemeral_x25519,
        hkdf_info,
        (random()?, *random::<12>()?),
    )
}

pub(crate) fn export_sealed_account_keys_inner(
    handle: &AccountKeysHandle,
    (preimage_hash, owner_signature): (B256, &[u8; 65]),
    requester: &[u8; 32],
    hkdf_info: &[u8],
    (ephemeral, nonce): (Zeroizing<[u8; 32]>, [u8; 12]),
) -> Result<SealedAccountKeysExport> {
    authorize_export(handle, preimage_hash, owner_signature)?;
    let shared = shared(&ephemeral, requester)?;
    let key = derive(requester, shared.as_ref(), hkdf_info)?;
    let plaintext = account_plaintext(handle);
    Ok(SealedAccountKeysExport {
        sealed: seal(&key, &nonce, plaintext.as_ref())?,
        nonce,
        enclave_ephemeral_pubkey: x25519_public(&ephemeral),
    })
}

/// Domain-neutral sealed box used by DKG shares and upgrade transfer.
#[allow(dead_code, reason = "Reserved for in-process DKG share sealing.")]
pub(crate) fn encrypt_share(
    recipient: &[u8; 32],
    info: &[u8],
    plaintext: &[u8],
) -> Result<crate::tee_encrypt::TeeSealedEnvelope> {
    let ephemeral = random()?;
    let nonce = *random::<12>()?;
    let shared = shared_legacy(&ephemeral, recipient);
    let key = derive(recipient, shared.as_ref(), info)?;
    Ok(crate::tee_encrypt::TeeSealedEnvelope {
        ephemeral_pub: x25519_public(&ephemeral),
        nonce,
        ciphertext: seal(&key, &nonce, plaintext)?,
    })
}

fn authorize_export(
    handle: &AccountKeysHandle,
    preimage_hash: B256,
    owner_signature: &[u8; 65],
) -> Result<()> {
    let recovered =
        outbe_primitives::tee_signatures::recover_signer(&preimage_hash, owner_signature)
            .map_err(TeeCryptoError::OwnerSignature)?;
    if recovered != handle.account() {
        return Err(TeeCryptoError::WrongOwner);
    }
    Ok(())
}
fn account_plaintext(handle: &AccountKeysHandle) -> Zeroizing<[u8; 64]> {
    let mut plaintext = Zeroizing::new([0; 64]);
    plaintext[..32].copy_from_slice(handle.view.0.as_ref());
    plaintext[32..].copy_from_slice(handle.modify.as_ref());
    plaintext
}
