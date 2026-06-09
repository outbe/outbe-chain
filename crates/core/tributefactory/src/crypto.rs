//! TEE cryptographic operations for tribute factory.
//!
//! Implements ECDHE (X25519) + HKDF-SHA256 + ChaCha20Poly1305 AEAD decryption
//! for encrypted tribute input payloads. Port of Cosmos `crypto.go`.
//!
//! - X25519 key agreement: `x25519-dalek` (supports static keys for TEE)
//! - HKDF-SHA256 + ChaCha20Poly1305: `ring` (no deprecated generic-array)
//!
//! TEE keys are loaded from environment variables at node startup,
//! NOT stored in EVM state.

use std::sync::OnceLock;

use ring::{
    aead,
    hkdf::{self, KeyType},
};
use x25519_dalek::{PublicKey, StaticSecret};

use outbe_common::WorldwideDay;
use serde::Deserialize;

/// TEE configuration holding X25519 key pair and HKDF salt.
pub struct TeeConfig {
    pub private_key: [u8; 32],
    pub public_key: [u8; 32],
    pub salt: [u8; 32],
}

static TEE_CONFIG: OnceLock<TeeConfig> = OnceLock::new();

/// Initialize TEE configuration from environment variables.
///
/// Reads `TEE_PRIVATE_KEY` (32-byte hex) and `TEE_SALT` (32-byte hex).
/// Derives the X25519 public key from the private key.
/// Must be called once at node startup.
pub fn init_tee_config() -> eyre::Result<()> {
    let pk_hex = std::env::var("TEE_PRIVATE_KEY")
        .map_err(|_| eyre::eyre!("TEE_PRIVATE_KEY env var not set"))?;
    let salt_hex =
        std::env::var("TEE_SALT").map_err(|_| eyre::eyre!("TEE_SALT env var not set"))?;

    let pk_bytes: [u8; 32] = hex::decode(pk_hex)?
        .try_into()
        .map_err(|_| eyre::eyre!("TEE_PRIVATE_KEY must be 32 bytes"))?;
    let salt_bytes: [u8; 32] = hex::decode(salt_hex)?
        .try_into()
        .map_err(|_| eyre::eyre!("TEE_SALT must be 32 bytes"))?;

    let secret = StaticSecret::from(pk_bytes);
    let public = PublicKey::from(&secret);

    TEE_CONFIG
        .set(TeeConfig {
            private_key: pk_bytes,
            public_key: public.to_bytes(),
            salt: salt_bytes,
        })
        .map_err(|_| eyre::eyre!("TEE config already initialized"))?;

    Ok(())
}

/// Returns the TEE configuration, if initialized.
pub fn tee_config() -> Option<&'static TeeConfig> {
    TEE_CONFIG.get()
}

/// Decrypted tribute input payload (matches Cosmos MsgTributeInputPayload).
#[derive(Debug, serde::Serialize, Deserialize)]
pub struct TributeInputPayload {
    pub creator: String,
    pub tribute_draft_id: String,
    pub worldwide_day: WorldwideDay,
    pub currency: u16,
    pub amount_base: String,
    pub amount_atto: String,
    pub su_hashes: Vec<String>,
    #[serde(default)]
    pub wallet_addresses: Vec<String>,
    #[serde(default)]
    pub sra_addresses: Vec<String>,
}

/// Decrypts an encrypted tribute input using ECDHE + HKDF + ChaCha20Poly1305.
///
/// Port of Cosmos `DecryptTributeInput` from `crypto.go`.
pub fn decrypt_tribute_input(
    cipher_text: &[u8],
    nonce_bytes: &[u8],
    ephemeral_pubkey: &[u8; 32],
) -> eyre::Result<TributeInputPayload> {
    // TEE enclave path: when a sidecar is configured, the offer is decrypted in
    // the enclave sidecar process (the offer key never lives in THIS node
    // process). The sidecar validates it and returns only the public draft
    // fields (creator / tribute_draft_id withheld). Falls through to the
    // in-process stub when no enclave is wired.
    // NOTE: "enclave" here is process isolation + Noise-IK, not SGX hardware —
    // quote/measurements/sealing are mocked under gramine-direct. Real SGX
    // confidentiality requires gramine-sgx.
    if crate::enclave_offer::is_enclave_configured() {
        return crate::enclave_offer::decrypt_offer_via_enclave(
            cipher_text,
            nonce_bytes,
            ephemeral_pubkey,
        );
    }

    let config = tee_config().ok_or_else(|| eyre::eyre!("TEE not configured"))?;

    if nonce_bytes.len() != 12 {
        return Err(eyre::eyre!("invalid nonce size (expected 12 bytes)"));
    }

    // ECDHE: X25519(tee_private_key, ephemeral_pubkey) → shared secret
    let secret = StaticSecret::from(config.private_key);
    let peer_public = PublicKey::from(*ephemeral_pubkey);
    let shared_secret = secret.diffie_hellman(&peer_public);

    // HKDF-SHA256: derive 32-byte encryption key
    let encryption_key = hkdf_derive(
        &config.salt,
        shared_secret.as_bytes(),
        b"tribute-factory-encryption",
    )?;

    // ChaCha20Poly1305: decrypt
    let nonce_arr: [u8; 12] = nonce_bytes
        .try_into()
        .map_err(|_| eyre::eyre!("invalid nonce length"))?;
    let plaintext = chacha20_poly1305_decrypt(&encryption_key, &nonce_arr, cipher_text)?;

    // Deserialize JSON
    let payload: TributeInputPayload = serde_json::from_slice(&plaintext)
        .map_err(|e| eyre::eyre!("failed to parse decrypted payload: {e}"))?;

    // Validate required fields
    if payload.tribute_draft_id.is_empty() {
        return Err(eyre::eyre!("tribute_draft_id is required"));
    }
    if payload.creator.is_empty() {
        return Err(eyre::eyre!("creator is required"));
    }
    if !payload.worldwide_day.is_valid() {
        return Err(eyre::eyre!("worldwide_day is invalid"));
    }
    if payload.su_hashes.is_empty() {
        return Err(eyre::eyre!("su_hashes cannot be empty"));
    }

    Ok(payload)
}

/// HKDF-SHA256: extract + expand to 32 bytes.
fn hkdf_derive(salt: &[u8], ikm: &[u8], info: &[u8]) -> eyre::Result<[u8; 32]> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(ikm);
    let info_refs: &[&[u8]] = &[info];
    let okm = prk
        .expand(info_refs, AeadKeyLen)
        .map_err(|_| eyre::eyre!("HKDF expansion failed"))?;
    let mut key = [0u8; 32];
    okm.fill(&mut key)
        .map_err(|_| eyre::eyre!("HKDF fill failed"))?;
    Ok(key)
}

/// KeyType for 32-byte AEAD keys.
struct AeadKeyLen;

impl KeyType for AeadKeyLen {
    fn len(&self) -> usize {
        32
    }
}

/// ChaCha20Poly1305 AEAD decrypt.
fn chacha20_poly1305_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> eyre::Result<Vec<u8>> {
    use aead::BoundKey;

    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| eyre::eyre!("failed to create ChaCha20Poly1305 key"))?;
    let mut opening_key = aead::OpeningKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = ciphertext.to_vec();
    let plaintext = opening_key
        .open_in_place(aead::Aad::empty(), &mut in_out)
        .map_err(|_| eyre::eyre!("decryption failed: invalid cipher or key"))?;
    Ok(plaintext.to_vec())
}

/// ChaCha20Poly1305 AEAD encrypt (used by tests).
#[cfg(test)]
pub(crate) fn chacha20_poly1305_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> eyre::Result<Vec<u8>> {
    use aead::BoundKey;

    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| eyre::eyre!("failed to create ChaCha20Poly1305 key"))?;
    let mut sealing_key = aead::SealingKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing_key
        .seal_in_place_append_tag(aead::Aad::empty(), &mut in_out)
        .map_err(|_| eyre::eyre!("encryption failed"))?;
    Ok(in_out)
}

use outbe_primitives::crypto::OneNonce;
