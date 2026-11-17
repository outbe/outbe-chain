//! Legacy sidecar envelope codecs, retained only until the sidecar is removed.
use crate::errors::{Result, TeeError};
use alloy_primitives::B256;
pub use outbe_enclave::crypto::*;
use outbe_tee::dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};
const DKG_SHARE_INFO: &[u8] = b"outbe/tee/dkg-share/v1";
const REGISTRY_NONCE_INFO: &[u8] = b"outbe/tee/registry-seal-nonce/v1";
const ONBOARDING_KEY_INFO_V1: &[u8] = b"outbe/tee/onboarding-key/v1";
const ONBOARDING_NONCE_INFO_V1: &[u8] = b"outbe/tee/onboarding-nonce/v1";
/// A DKG share sealed to a recipient enclave's X25519 key: a fresh ephemeral
/// public key, a nonce, and the ChaCha20Poly1305 ciphertext. The host only ever
/// relays this opaque blob. The plaintext share exists only inside the dealer's
/// and the recipient's enclaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedShare {
    pub ephemeral_pub: [u8; 32],
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

impl EncryptedShare {
    /// Frozen flat wire encoding: public key, nonce, authenticated ciphertext.
    pub fn to_bytes(&self) -> Vec<u8> {
        [
            self.ephemeral_pub.as_slice(),
            self.nonce.as_slice(),
            self.ciphertext.as_slice(),
        ]
        .concat()
    }
    /// Reject truncated headers before parsing the frozen flat encoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let header = bytes.get(..44).ok_or(TeeError::DecryptFailed)?;
        Ok(Self {
            ephemeral_pub: header[..32]
                .try_into()
                .map_err(|_| TeeError::DecryptFailed)?,
            nonce: header[32..]
                .try_into()
                .map_err(|_| TeeError::DecryptFailed)?,
            ciphertext: bytes[44..].to_vec(),
        })
    }
}

fn seal_share(
    sender: &StaticSecret,
    recipient: &[u8; 32],
    nonce: [u8; 12],
    plaintext: &[u8],
) -> Result<EncryptedShare> {
    let shared = sender.diffie_hellman(&PublicKey::from(*recipient));
    let key = Zeroizing::new(hkdf_sha256(recipient, shared.as_bytes(), DKG_SHARE_INFO)?);
    Ok(EncryptedShare {
        ephemeral_pub: PublicKey::from(sender).to_bytes(),
        nonce,
        ciphertext: chacha20poly1305_encrypt(&key, &nonce, plaintext)?,
    })
}
fn derive_nonce(salt: &[u8], secret: &[u8; 32], label: &[u8]) -> Result<[u8; 12]> {
    let bytes = Zeroizing::new(hkdf_sha256(salt, secret, label)?);
    let mut nonce = [0; 12];
    nonce.copy_from_slice(&bytes[..12]);
    Ok(nonce)
}

/// Seal `plaintext` (a serialized DKG share) to `recipient_pub` with a fresh
/// ephemeral X25519 keypair (sealed-box). The recipient recovers it with
/// [`decrypt_share`]. The shared key is `HKDF(salt = recipient_pub, ikm = ECDHE,
/// info = DKG_SHARE_INFO)`. A fresh ephemeral key per call makes the key unique,
/// so the random nonce is defense-in-depth.
///
/// `OsRng` here is **transport-encryption** randomness (ephemeral X25519 +
/// nonce). It is NOT consensus randomness. The encrypted share decrypts to a
/// deterministic plaintext. Ciphertext freshness only protects confidentiality
/// in flight. It never feeds VRF/leader-election/state transitions.
pub fn encrypt_share(recipient_pub: &[u8; 32], plaintext: &[u8]) -> Result<EncryptedShare> {
    use rand_core::RngCore;
    let mut ephemeral_secret = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut ephemeral_secret);
    let mut nonce = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut nonce);

    let sender = StaticSecret::from(ephemeral_secret);
    ephemeral_secret.zeroize();
    seal_share(&sender, recipient_pub, nonce, plaintext)
}

/// DETERMINISTIC seal of `plaintext` to `recipient_pub`, suitable for committing
/// the sealed blob ON-CHAIN (an encrypted-seed analog).
/// `sender_static_secret` is the resident offer key's X25519 secret, identical
/// across the committee. Every enclave that holds it produces a BYTE-IDENTICAL
/// [`EncryptedShare`] for the same recipient + plaintext. So the result can be a
/// consensus-validated on-chain artifact.
///
/// Unlike [`encrypt_share`] (random ephemeral key + random nonce), this function
/// uses static-static ECDH between the sender's static secret and the recipient's
/// static public. It derives the nonce from the shared secret, with NO RNG. The
/// recipient opens it with the UNCHANGED [`decrypt_share`] using its own X25519
/// secret and the sender's static public. `ephemeral_pub` carries that static
/// public, which equals the on-chain `tribute_offer_public_key`.
///
/// The fixed-per-pair nonce is safe. The AEAD key is unique per (offer-key epoch,
/// recipient), because the offer secret rotates with the key epoch and
/// `recipient_pub` salts both key and nonce.
pub fn encrypt_share_deterministic(
    sender_static_secret: &[u8; 32],
    recipient_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedShare> {
    let sender = StaticSecret::from(*sender_static_secret);
    let shared = sender.diffie_hellman(&PublicKey::from(*recipient_pub));
    let nonce = derive_nonce(recipient_pub, shared.as_bytes(), REGISTRY_NONCE_INFO)?;
    seal_share(&sender, recipient_pub, nonce, plaintext)
}

/// Open a share sealed by [`encrypt_share`] with the recipient's X25519 private
/// key. This function recomputes the salt (`recipient_pub`) from
/// `recipient_secret`, so it matches the sealer without being transmitted.
pub fn decrypt_share(recipient_secret: &[u8; 32], enc: &EncryptedShare) -> Result<Vec<u8>> {
    let secret = StaticSecret::from(*recipient_secret);
    let recipient_pub = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(enc.ephemeral_pub));
    let key = hkdf_sha256(&recipient_pub, shared.as_bytes(), DKG_SHARE_INFO)?;
    chacha20poly1305_decrypt(&key, &enc.nonce, &enc.ciphertext)
}

fn onboarding_aead_material(
    context_hash: B256,
    local_secret: &[u8; 32],
    peer_public: &[u8; 32],
) -> Result<([u8; 32], [u8; 12])> {
    let shared = StaticSecret::from(*local_secret).diffie_hellman(&PublicKey::from(*peer_public));
    let shared_secret = shared.as_bytes();
    let key = hkdf_sha256(
        context_hash.as_slice(),
        shared_secret,
        ONBOARDING_KEY_INFO_V1,
    )?;
    let nonce = derive_nonce(
        context_hash.as_slice(),
        shared_secret,
        ONBOARDING_NONCE_INFO_V1,
    )?;
    Ok((key, nonce))
}

/// Deterministically encrypt the resident founding group signature for one
/// exact accepted registration context. No caller-selected plaintext exists.
pub fn encrypt_onboarding_artifact_v1(
    sender_offer_secret: &[u8; 32],
    context: DcapOnboardingContextV1,
    group_signature: &[u8],
) -> Result<DcapOnboardingArtifactV1> {
    let sender = StaticSecret::from(*sender_offer_secret);
    if PublicKey::from(&sender).to_bytes() != context.network_encryption_public {
        return Err(TeeError::Dkg(
            "onboarding context offer public does not match resident key".into(),
        ));
    }
    let (key, nonce) = onboarding_aead_material(
        context.context_hash(),
        sender_offer_secret,
        &context.recipient_x25519,
    )?;
    encoded_artifact(
        context,
        nonce,
        chacha20poly1305_encrypt(&key, &nonce, group_signature)?,
    )
}

fn encoded_artifact(
    context: DcapOnboardingContextV1,
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
) -> Result<DcapOnboardingArtifactV1> {
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce,
        ciphertext,
    };
    artifact
        .encode_canonical()
        .map_err(|_| TeeError::EncryptFailed)?;
    Ok(artifact)
}

/// Open one purpose-bound onboarding artifact. The target derives its recipient
/// public key, context key and nonce locally. The function rejects a wire nonce
/// mismatch before AEAD open. It never uses the wire nonce as nonce authority.
pub fn decrypt_onboarding_artifact_v1(
    recipient_secret: &[u8; 32],
    artifact: &DcapOnboardingArtifactV1,
) -> Result<Zeroizing<Vec<u8>>> {
    let recipient = StaticSecret::from(*recipient_secret);
    let recipient_public = PublicKey::from(&recipient).to_bytes();
    if recipient_public != artifact.context.recipient_x25519 {
        return Err(TeeError::DecryptFailed);
    }
    let (key, expected_nonce) = onboarding_aead_material(
        artifact.context.context_hash(),
        recipient_secret,
        &artifact.context.network_encryption_public,
    )?;
    if artifact.nonce != expected_nonce {
        return Err(TeeError::DecryptFailed);
    }
    Ok(Zeroizing::new(chacha20poly1305_decrypt(
        &key,
        &expected_nonce,
        &artifact.ciphertext,
    )?))
}

/// Derive the X25519 public key for a 32-byte X25519 secret (the enclave's DKG
/// share-decryption key). Announced so dealers can seal shares to this enclave.
pub fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(*secret)).to_bytes()
}

/// Derive the legacy offer secret for sealed sidecar restart state.
pub fn derive_tribute_offer_secret_from_group_sig(
    group_sig: &[u8],
    chain_id: B256,
    epoch: u64,
) -> Result<(Zeroizing<[u8; 32]>, [u8; 32])> {
    let mut info = b"outbe/tribute/v1/".to_vec();
    info.extend_from_slice(epoch.to_string().as_bytes());
    let seed = Zeroizing::new(hkdf_sha256(chain_id.as_slice(), group_sig, &info)?);
    let secret = Zeroizing::new(hkdf_sha256(&*seed, b"", b"outbe/tribute/offer-x25519/v1")?);
    let public = x25519_public(&secret);
    Ok((secret, public))
}
