//! Historical raw-secret codecs for fixed development/golden fixtures only.
use crate::{
    crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256},
    errors::{Result, TeeError},
};
use alloy_primitives::B256;
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
    let sender_pub = PublicKey::from(&sender).to_bytes();
    let shared = sender.diffie_hellman(&PublicKey::from(*recipient_pub));
    let key = hkdf_sha256(recipient_pub, shared.as_bytes(), DKG_SHARE_INFO)?;
    let nonce_okm = hkdf_sha256(recipient_pub, shared.as_bytes(), REGISTRY_NONCE_INFO)?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_okm[..12]);
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, plaintext)?;
    Ok(EncryptedShare {
        ephemeral_pub: sender_pub,
        nonce,
        ciphertext,
    })
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
    shared_secret: &[u8; 32],
) -> Result<([u8; 32], [u8; 12])> {
    let key = hkdf_sha256(
        context_hash.as_slice(),
        shared_secret,
        ONBOARDING_KEY_INFO_V1,
    )?;
    let nonce_okm = hkdf_sha256(
        context_hash.as_slice(),
        shared_secret,
        ONBOARDING_NONCE_INFO_V1,
    )?;
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&nonce_okm[..12]);
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
        return Err(TeeError::OnboardingFixtureOfferKeyMismatch);
    }
    let shared = sender.diffie_hellman(&PublicKey::from(context.recipient_x25519));
    let (key, nonce) = onboarding_aead_material(context.context_hash(), shared.as_bytes())?;
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, group_signature)?;
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
    let shared =
        recipient.diffie_hellman(&PublicKey::from(artifact.context.network_encryption_public));
    let (key, expected_nonce) =
        onboarding_aead_material(artifact.context.context_hash(), shared.as_bytes())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{decrypt_tee_amount, ecdhe_tribute_offer_decrypt};
    use alloy_primitives::Address;
    use outbe_tee::resident::OfferKeyHandle;

    /// The shared client-side encryptor (`crate::offer_encrypt`, used by
    /// outbe-cli, the bench and the node canary) must round-trip with THIS
    /// decrypt path byte-for-byte. The canary asserts exactly this in prod.
    #[test]
    fn shared_offer_encrypt_helper_round_trips_with_decrypt() {
        let offer_sk = [7u8; 32];
        let offer_pub = x25519_public(&offer_sk);
        let eph_sk = [9u8; 32];
        let nonce = [1u8; 12];
        let plaintext = crate::offer_encrypt::canary_offer_json(20250115);
        let cipher_text = crate::offer_encrypt::encrypt_tribute_offer_with(
            &offer_pub,
            eph_sk,
            nonce,
            plaintext.as_bytes(),
        )
        .expect("encrypt");
        let eph_pub = x25519_public(&eph_sk);
        let decrypted = ecdhe_tribute_offer_decrypt(
            &OfferKeyHandle::for_test(offer_sk),
            &crate::OFFER_HKDF_SALT,
            &eph_pub,
            &nonce,
            &cipher_text,
        )
        .expect("decrypt");
        assert_eq!(decrypted, plaintext.as_bytes());
    }

    fn onboarding_context(
        recipient_x25519: [u8; 32],
        tribute_offer_public: [u8; 32],
    ) -> DcapOnboardingContextV1 {
        DcapOnboardingContextV1 {
            chain_id: [0x21; 32],
            genesis_hash: B256::repeat_byte(0x22),
            intent_hash: B256::repeat_byte(0x23),
            node_id_hash: B256::repeat_byte(0x24),
            enclave_id: B256::repeat_byte(0x25),
            binding_id: B256::repeat_byte(0x26),
            policy_hash: B256::repeat_byte(0x27),
            recipient_x25519,
            network_encryption_public: tribute_offer_public,
            key_epoch: 3,
            network_encryption_epoch: 4,
        }
    }

    /// The deterministic registry seal must be byte-identical across calls (so every
    /// committee enclave commits the same on-chain blob) and must round-trip through
    /// the UNCHANGED `decrypt_share` using only the recipient's secret.
    #[test]
    fn encrypt_share_deterministic_is_byte_identical_and_roundtrips() {
        let offer_secret = [7u8; 32];
        let recipient_secret = [9u8; 32];
        let recipient_pub = x25519_public(&recipient_secret);
        let plaintext = b"resident group signature bytes";

        let a = encrypt_share_deterministic(&offer_secret, &recipient_pub, plaintext).unwrap();
        let b = encrypt_share_deterministic(&offer_secret, &recipient_pub, plaintext).unwrap();
        assert_eq!(
            a.to_bytes(),
            b.to_bytes(),
            "deterministic seal must be byte-identical for on-chain commit"
        );

        // `ephemeral_pub` carries the sender's STATIC public = the offer public key,
        // so the recipient (with only its own secret) opens it via decrypt_share.
        assert_eq!(a.ephemeral_pub, x25519_public(&offer_secret));
        let opened = decrypt_share(&recipient_secret, &a).unwrap();
        assert_eq!(opened.as_slice(), plaintext);

        // Different recipient -> different blob (key + nonce both salted by recipient).
        let other_pub = x25519_public(&[11u8; 32]);
        let c = encrypt_share_deterministic(&offer_secret, &other_pub, plaintext).unwrap();
        assert_ne!(a.to_bytes(), c.to_bytes());
    }

    #[test]
    fn onboarding_artifact_is_exact_replay_and_context_separated() {
        let offer_secret = [0x31; 32];
        let offer_public = x25519_public(&offer_secret);
        let recipient_secret = [0x32; 32];
        let recipient_public = x25519_public(&recipient_secret);
        let context = onboarding_context(recipient_public, offer_public);
        let plaintext = b"exact founding group signature";

        let first = encrypt_onboarding_artifact_v1(&offer_secret, context, plaintext).unwrap();
        let replay = encrypt_onboarding_artifact_v1(&offer_secret, context, plaintext).unwrap();
        assert_eq!(first, replay);
        assert_eq!(
            decrypt_onboarding_artifact_v1(&recipient_secret, &first)
                .unwrap()
                .as_slice(),
            plaintext
        );

        let mut changed_context = context;
        changed_context.intent_hash = B256::repeat_byte(0x41);
        let changed =
            encrypt_onboarding_artifact_v1(&offer_secret, changed_context, plaintext).unwrap();
        assert_ne!(first.nonce, changed.nonce);
        assert_ne!(first.ciphertext, changed.ciphertext);
    }

    #[test]
    fn onboarding_ingest_rejects_wire_nonce_before_aead_open() {
        let offer_secret = [0x51; 32];
        let offer_public = x25519_public(&offer_secret);
        let recipient_secret = [0x52; 32];
        let recipient_public = x25519_public(&recipient_secret);
        let mut artifact = encrypt_onboarding_artifact_v1(
            &offer_secret,
            onboarding_context(recipient_public, offer_public),
            b"group signature",
        )
        .unwrap();
        artifact.nonce[0] ^= 1;
        assert!(matches!(
            decrypt_onboarding_artifact_v1(&recipient_secret, &artifact),
            Err(TeeError::DecryptFailed)
        ));
    }

    #[test]
    fn onboarding_domains_are_distinct_from_existing_share_domains() {
        assert_ne!(ONBOARDING_KEY_INFO_V1, ONBOARDING_NONCE_INFO_V1);
        assert_ne!(ONBOARDING_KEY_INFO_V1, DKG_SHARE_INFO);
        assert_ne!(ONBOARDING_KEY_INFO_V1, REGISTRY_NONCE_INFO);
        assert_ne!(ONBOARDING_NONCE_INFO_V1, DKG_SHARE_INFO);
        assert_ne!(ONBOARDING_NONCE_INFO_V1, REGISTRY_NONCE_INFO);
    }

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
        let key = hkdf_sha256(
            &salt,
            shared.as_bytes(),
            crate::offer_encrypt::OFFER_HKDF_INFO,
        )
        .unwrap();
        let nonce = [1u8; 12];
        let plaintext = b"{\"hello\":\"tribute\"}";
        let ciphertext = chacha20poly1305_encrypt(&key, &nonce, plaintext).unwrap();

        // Enclave side: shared = tribute_offer_sk x eph_pub.
        let decrypted = ecdhe_tribute_offer_decrypt(
            &OfferKeyHandle::for_test(tribute_offer_sk),
            &salt,
            &eph_pub,
            &nonce,
            &ciphertext,
        )
        .unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn reject_bad_nonce_len() {
        let err = ecdhe_tribute_offer_decrypt(
            &OfferKeyHandle::for_test([0u8; 32]),
            &[0u8; 32],
            &[0u8; 32],
            &[0u8; 11],
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, TeeError::InvalidNonce(11)));
    }

    #[test]
    fn dkg_share_sealed_box_roundtrips() {
        // Recipient enclave's DKG share-decryption keypair.
        let recipient_sk = [0x33u8; 32];
        let recipient_pub = x25519_public(&recipient_sk);

        let share = b"a-serialized-dkg-share-scalar-bytes";
        let sealed = encrypt_share(&recipient_pub, share).unwrap();
        // The host never sees the plaintext - only the opaque blob.
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

    /// `to_bytes` must lay out exactly `ephemeral_pub(32) || nonce(12) ||
    /// ciphertext`, and `from_bytes` must read those same slices back. Uses
    /// distinct, hand-picked bytes per field so a swapped/overlapping range fails.
    #[test]
    fn encrypted_share_wire_layout_is_exact() {
        let share = EncryptedShare {
            ephemeral_pub: [0xAAu8; 32],
            nonce: [0xBBu8; 12],
            ciphertext: vec![0xCC, 0xDD, 0xEE],
        };
        let wire = share.to_bytes();

        assert_eq!(wire.len(), 44 + 3);
        assert_eq!(&wire[..32], &[0xAAu8; 32]); // ephemeral_pub
        assert_eq!(&wire[32..44], &[0xBBu8; 12]); // nonce
        assert_eq!(&wire[44..], &[0xCC, 0xDD, 0xEE]); // ciphertext

        // Round-trips without touching the crypto path.
        assert_eq!(EncryptedShare::from_bytes(&wire).unwrap(), share);
    }

    /// The length check is `< 44`, so exactly 44 bytes is valid and decodes to an
    /// empty ciphertext - the boundary case.
    #[test]
    fn encrypted_share_from_bytes_accepts_min_len_empty_ciphertext() {
        let share = EncryptedShare {
            ephemeral_pub: [1u8; 32],
            nonce: [2u8; 12],
            ciphertext: Vec::new(),
        };
        let wire = share.to_bytes();
        assert_eq!(wire.len(), 44);
        let parsed = EncryptedShare::from_bytes(&wire).unwrap();
        assert_eq!(parsed, share);
        assert!(parsed.ciphertext.is_empty());
    }

    /// Standalone AEAD round-trip plus tamper detection: flipping any ciphertext
    /// or tag byte, or opening with the wrong nonce/key, must fail closed.
    #[test]
    fn chacha20poly1305_roundtrip_and_tamper_rejected() {
        let key = [0x42u8; 32];
        let nonce = [0x24u8; 12];
        let plaintext = b"decode me";

        let mut ct = chacha20poly1305_encrypt(&key, &nonce, plaintext).unwrap();
        // seal appends a 16-byte Poly1305 tag.
        assert_eq!(ct.len(), plaintext.len() + 16);
        assert_eq!(
            chacha20poly1305_decrypt(&key, &nonce, &ct).unwrap(),
            plaintext
        );

        // Tampered ciphertext (flip a tag byte) -> DecryptFailed.
        let last = ct.len() - 1;
        ct[last] ^= 0x01;
        assert!(matches!(
            chacha20poly1305_decrypt(&key, &nonce, &ct),
            Err(TeeError::DecryptFailed)
        ));
        ct[last] ^= 0x01; // restore

        // Wrong nonce and wrong key both fail.
        assert!(chacha20poly1305_decrypt(&key, &[0u8; 12], &ct).is_err());
        assert!(chacha20poly1305_decrypt(&[0u8; 32], &nonce, &ct).is_err());
    }

    #[test]
    fn decrypt_tee_amount_round_trips_only_for_the_bound_account() {
        let offer_sk = [7u8; 32];
        let offer_pub = x25519_public(&offer_sk);
        let chain = B256::repeat_byte(0x11);
        let alice = Address::repeat_byte(0x22);
        let eve = Address::repeat_byte(0x33);
        let amount = alloy_primitives::U256::from(1000u64);
        let encrypted = crate::offer_encrypt::encrypt_tee_amount_with(
            &offer_pub,
            (chain, alice),
            amount,
            [9u8; 32],
            [1u8; 12],
        )
        .expect("encrypt");
        assert_eq!(
            decrypt_tee_amount(
                &OfferKeyHandle::for_test(offer_sk),
                &encrypted,
                chain,
                alice
            )
            .unwrap(),
            amount
        );
        assert!(matches!(
            decrypt_tee_amount(&OfferKeyHandle::for_test(offer_sk), &encrypted, chain, eve),
            Err(TeeError::Resident(_))
        ));
    }
}
