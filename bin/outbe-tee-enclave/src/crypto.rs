//! Enclave crypto core (secret-bearing — lives only in the enclave binary).
//!
//! Two responsibilities for slice 1:
//!
//! 1. **Offer decryption primitive** — byte-identical to the host's current
//!    `outbe-tributefactory` `crypto.rs`: ECDHE(X25519) + HKDF-SHA256 +
//!    ChaCha20Poly1305 with the same `b"tribute-factory-encryption"` info
//!    label. This is what `crypto.rs` will call into the enclave for once the
//!    decrypt key is enclave-resident.
//!
//! 2. **Tribute-offer-key derivation** — DKG group threshold signature -> HKDF ->
//!    tribute-offer X25519 keypair, resident in each enclave so every validator
//!    decrypts tribute offers deterministically during block execution.
//!
//! `ring` (HKDF + AEAD) and `x25519-dalek` mirror the existing host code, so
//! ciphertext produced by current clients decrypts identically here.

use ring::{
    aead,
    hkdf::{self, KeyType},
};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use alloy_primitives::B256;

use crate::errors::{Result, TeeError};

/// HKDF info label for the offer encryption key. MUST match
/// `outbe-tributefactory::crypto` for byte-identical decryption.
const OFFER_AEAD_INFO: &[u8] = b"tribute-factory-encryption";
/// HKDF info label for deriving the offer X25519 secret from the root seed.
const OFFER_X25519_INFO: &[u8] = b"outbe/tribute/offer-x25519/v1";
/// Offer-key HKDF info prefix: `info = "outbe/tribute/v1/" || epoch`.
const OFFER_SEED_INFO_PREFIX: &[u8] = b"outbe/tribute/v1/";
/// HKDF info label for DKG share sealed-box encryption. TEE infrastructure
/// (not tribute-offer-specific), so the domain is `outbe/tee/...`; distinct from
/// offer encryption so a share key can never be derived as an offer key.
const DKG_SHARE_INFO: &[u8] = b"outbe/tee/dkg-share/v1";

/// Single-use nonce provider for ring's `BoundKey` API. Replicated locally
/// (mirrors `outbe_primitives::crypto::OneNonce`) to keep enclave dependencies
/// minimal for reproducible `MRENCLAVE`.
pub(crate) struct OneNonce([u8; 12]);

impl OneNonce {
    pub(crate) fn new(nonce: [u8; 12]) -> Self {
        Self(nonce)
    }
}

impl aead::NonceSequence for OneNonce {
    fn advance(&mut self) -> core::result::Result<aead::Nonce, ring::error::Unspecified> {
        Ok(aead::Nonce::assume_unique_for_key(self.0))
    }
}

/// `KeyType` for 32-byte HKDF output.
struct Len32;
impl KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// HKDF-SHA256 extract + expand to 32 bytes.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(ikm);
    let info_refs: &[&[u8]] = &[info];
    let okm = prk
        .expand(info_refs, Len32)
        .map_err(|_| TeeError::HkdfFailed)?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(|_| TeeError::HkdfFailed)?;
    Ok(out)
}

/// ChaCha20Poly1305 AEAD decrypt (empty AAD), matching host semantics.
pub fn chacha20poly1305_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    use aead::BoundKey;
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| TeeError::DecryptFailed)?;
    let mut opening = aead::OpeningKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = ciphertext.to_vec();
    let plaintext = opening
        .open_in_place(aead::Aad::empty(), &mut in_out)
        .map_err(|_| TeeError::DecryptFailed)?;
    Ok(plaintext.to_vec())
}

/// ChaCha20Poly1305 AEAD encrypt (empty AAD). Used by the client/host and tests
/// to produce ciphertext; the enclave itself only decrypts offers.
pub fn chacha20poly1305_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    use aead::BoundKey;
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| TeeError::EncryptFailed)?;
    let mut sealing = aead::SealingKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Aad::empty(), &mut in_out)
        .map_err(|_| TeeError::EncryptFailed)?;
    Ok(in_out)
}

/// Offer decryption primitive: ECDHE(static_secret, ephemeral_pubkey) ->
/// HKDF-SHA256(salt, shared, info) -> ChaCha20Poly1305 decrypt.
///
/// Byte-identical to `outbe-tributefactory::crypto::decrypt_tribute_input`'s
/// cryptographic core (this returns raw plaintext; payload parsing is a later
/// slice).
pub fn ecdhe_tribute_offer_decrypt(
    tribute_offer_private_key: &[u8; 32],
    salt: &[u8; 32],
    ephemeral_pubkey: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    if nonce.len() != 12 {
        return Err(TeeError::InvalidNonce(nonce.len()));
    }
    let secret = StaticSecret::from(*tribute_offer_private_key);
    let peer_public = PublicKey::from(*ephemeral_pubkey);
    let shared_secret = secret.diffie_hellman(&peer_public);

    let encryption_key = hkdf_sha256(salt, shared_secret.as_bytes(), OFFER_AEAD_INFO)?;

    let mut nonce_arr = [0u8; 12];
    nonce_arr.copy_from_slice(nonce);
    chacha20poly1305_decrypt(&encryption_key, &nonce_arr, ciphertext)
}

/// Derive the tribute-offer X25519 keypair from a 32-byte seed.
///
/// Returns `(secret_bytes, public_bytes)`. The caller is responsible for
/// zeroizing `secret_bytes` (callers wrap it in a zeroizing holder).
pub fn derive_tribute_offer_keypair(seed: &[u8; 32]) -> Result<([u8; 32], [u8; 32])> {
    let secret_bytes = hkdf_sha256(seed, b"", OFFER_X25519_INFO)?;
    let secret = StaticSecret::from(secret_bytes);
    let public = PublicKey::from(&secret);
    Ok((secret_bytes, public.to_bytes()))
}

/// Derive the tribute-offer X25519 keypair from the DKG **group threshold
/// signature** `group_sig` — the signature every enclave recovers (via
/// `threshold::recover`) from `2f+1` partial signatures over the fixed offer
/// message. `group_sig` is the shared, deterministic secret material:
/// byte-identical on every honest enclave (so all derive the same tribute-offer
/// key), yet unforgeable without a threshold of DKG shares. `chain_id` + `epoch`
/// are bound into the HKDF for domain separation. The resulting tribute-offer
/// secret is resident in each enclave so every validator decrypts tribute offers
/// deterministically during block execution — an architectural requirement of
/// deterministic local re-execution, not a compromise. Returns
/// `(tribute_offer_secret, tribute_offer_public)`; the caller zeroizes the secret.
pub fn derive_tribute_offer_secret_from_group_sig(
    group_sig: &[u8],
    chain_id: B256,
    epoch: u64,
) -> Result<([u8; 32], [u8; 32])> {
    let mut info = OFFER_SEED_INFO_PREFIX.to_vec();
    info.extend_from_slice(epoch.to_string().as_bytes());
    let seed = hkdf_sha256(chain_id.as_slice(), group_sig, &info)?;
    derive_tribute_offer_keypair(&seed)
}

/// A DKG share sealed to a recipient enclave's X25519 key: a fresh ephemeral
/// public key, a nonce, and the ChaCha20Poly1305 ciphertext. The host only ever
/// relays this opaque blob; the plaintext share exists only inside the dealer's
/// and the recipient's enclaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedShare {
    pub ephemeral_pub: [u8; 32],
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

impl EncryptedShare {
    /// Flat wire encoding `ephemeral_pub(32) || nonce(12) || ciphertext`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(44 + self.ciphertext.len());
        out.extend_from_slice(&self.ephemeral_pub);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    /// Decode the flat wire encoding produced by [`EncryptedShare::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 44 {
            return Err(TeeError::DecryptFailed);
        }
        let mut ephemeral_pub = [0u8; 32];
        ephemeral_pub.copy_from_slice(&bytes[..32]);
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&bytes[32..44]);
        Ok(Self {
            ephemeral_pub,
            nonce,
            ciphertext: bytes[44..].to_vec(),
        })
    }
}

/// Seal `plaintext` (a serialized DKG share) to `recipient_pub` with a fresh
/// ephemeral X25519 keypair (sealed-box). The recipient recovers it with
/// [`decrypt_share`]. The shared key is `HKDF(salt = recipient_pub, ikm = ECDHE,
/// info = DKG_SHARE_INFO)`; a fresh ephemeral key per call makes the key unique,
/// so the random nonce is defense-in-depth.
///
/// `OsRng` here is **transport-encryption** randomness (ephemeral X25519 + nonce)
/// — it is NOT consensus randomness. The encrypted share decrypts to a
/// deterministic plaintext; ciphertext freshness only protects confidentiality
/// in flight, and never feeds VRF/leader-election/state transitions.
pub fn encrypt_share(recipient_pub: &[u8; 32], plaintext: &[u8]) -> Result<EncryptedShare> {
    use rand_core::RngCore;
    let mut ephemeral_secret = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut ephemeral_secret);
    let mut nonce = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut nonce);

    let eph_secret = StaticSecret::from(ephemeral_secret);
    ephemeral_secret.zeroize();
    let ephemeral_pub = PublicKey::from(&eph_secret).to_bytes();
    let shared = eph_secret.diffie_hellman(&PublicKey::from(*recipient_pub));
    let key = hkdf_sha256(recipient_pub, shared.as_bytes(), DKG_SHARE_INFO)?;
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, plaintext)?;
    Ok(EncryptedShare {
        ephemeral_pub,
        nonce,
        ciphertext,
    })
}

/// Open a share sealed by [`encrypt_share`] with the recipient's X25519 private
/// key. The salt (`recipient_pub`) is recomputed from `recipient_secret`, so it
/// matches the sealer without being transmitted.
pub fn decrypt_share(recipient_secret: &[u8; 32], enc: &EncryptedShare) -> Result<Vec<u8>> {
    let secret = StaticSecret::from(*recipient_secret);
    let recipient_pub = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(enc.ephemeral_pub));
    let key = hkdf_sha256(&recipient_pub, shared.as_bytes(), DKG_SHARE_INFO)?;
    chacha20poly1305_decrypt(&key, &enc.nonce, &enc.ciphertext)
}

/// Derive the X25519 public key for a 32-byte X25519 secret (the enclave's DKG
/// share-decryption key). Announced so dealers can seal shares to this enclave.
pub fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(*secret)).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encrypt as a client would (ephemeral_secret x tribute_offer_public), then decrypt
    /// in the enclave (tribute_offer_secret x ephemeral_public). Proves the ECDHE+HKDF+
    /// ChaCha core round-trips, i.e. real client ciphertext decrypts here.
    #[test]
    fn ecdhe_tribute_offer_roundtrip() {
        let tribute_offer_sk = [7u8; 32];
        let tribute_offer_pub = PublicKey::from(&StaticSecret::from(tribute_offer_sk)).to_bytes();

        let eph_sk = [9u8; 32];
        let eph_pub = PublicKey::from(&StaticSecret::from(eph_sk)).to_bytes();

        // Client side: shared = eph_sk x tribute_offer_pub.
        let shared = StaticSecret::from(eph_sk).diffie_hellman(&PublicKey::from(tribute_offer_pub));
        let salt = [3u8; 32];
        let key = hkdf_sha256(&salt, shared.as_bytes(), OFFER_AEAD_INFO).unwrap();
        let nonce = [1u8; 12];
        let plaintext = b"{\"hello\":\"tribute\"}";
        let ciphertext = chacha20poly1305_encrypt(&key, &nonce, plaintext).unwrap();

        // Enclave side: shared = tribute_offer_sk x eph_pub.
        let decrypted =
            ecdhe_tribute_offer_decrypt(&tribute_offer_sk, &salt, &eph_pub, &nonce, &ciphertext)
                .unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn reject_bad_nonce_len() {
        let err = ecdhe_tribute_offer_decrypt(&[0u8; 32], &[0u8; 32], &[0u8; 32], &[0u8; 11], &[])
            .unwrap_err();
        assert!(matches!(err, TeeError::InvalidNonce(11)));
    }

    #[test]
    fn tribute_offer_secret_is_deterministic_and_chain_bound() {
        // The group threshold signature is the shared secret material every
        // enclave recovers identically; HKDF binds chain_id + epoch.
        let sig = b"a-recovered-group-threshold-signature-~48-bytes";
        let cid_a = B256::repeat_byte(0xAA);
        let cid_b = B256::repeat_byte(0xBB);

        let (a1, _) = derive_tribute_offer_secret_from_group_sig(sig, cid_a, 0).unwrap();
        let (a2, _) = derive_tribute_offer_secret_from_group_sig(sig, cid_a, 0).unwrap();
        let (b1, _) = derive_tribute_offer_secret_from_group_sig(sig, cid_b, 0).unwrap();
        assert_eq!(a1, a2, "same inputs -> same offer secret");
        assert_ne!(a1, b1, "different chain_id -> different offer secret");
    }

    #[test]
    fn dkg_share_sealed_box_roundtrips() {
        // Recipient enclave's DKG share-decryption keypair.
        let recipient_sk = [0x33u8; 32];
        let recipient_pub = x25519_public(&recipient_sk);

        let share = b"a-serialized-dkg-share-scalar-bytes";
        let sealed = encrypt_share(&recipient_pub, share).unwrap();
        // The host never sees the plaintext — only the opaque blob.
        assert_ne!(sealed.ciphertext.as_slice(), share.as_slice());

        let opened = decrypt_share(&recipient_sk, &sealed).unwrap();
        assert_eq!(opened, share);

        // Wire round-trip of the opaque blob.
        let wire = sealed.to_bytes();
        let parsed = EncryptedShare::from_bytes(&wire).unwrap();
        assert_eq!(parsed, sealed);
        assert_eq!(decrypt_share(&recipient_sk, &parsed).unwrap(), share);
    }

    #[test]
    fn dkg_share_rejects_wrong_recipient() {
        let recipient_pub = x25519_public(&[0x33u8; 32]);
        let wrong_sk = [0x44u8; 32];
        let sealed = encrypt_share(&recipient_pub, b"share").unwrap();
        // A different recipient key cannot open the sealed share.
        assert!(decrypt_share(&wrong_sk, &sealed).is_err());
    }

    #[test]
    fn dkg_share_from_bytes_rejects_truncated() {
        assert!(EncryptedShare::from_bytes(&[0u8; 43]).is_err());
    }

    #[test]
    fn tribute_offer_keypair_is_deterministic() {
        let seed = [1u8; 32];
        let (sk1, pk1) = derive_tribute_offer_keypair(&seed).unwrap();
        let (sk2, pk2) = derive_tribute_offer_keypair(&seed).unwrap();
        assert_eq!(sk1, sk2);
        assert_eq!(pk1, pk2);
        // Public key matches the secret.
        assert_eq!(pk1, PublicKey::from(&StaticSecret::from(sk1)).to_bytes());
    }
}
