//! Deterministic onboarding seal for frozen golden vectors.
use outbe_core_common::confidential_tee::{chacha20poly1305_encrypt, hkdf_sha256, TeeError};
use outbe_tee::dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1};
use x25519_dalek::{PublicKey, StaticSecret};

const ONBOARDING_KEY_INFO_V1: &[u8] = b"outbe/tee/onboarding-key/v1";
const ONBOARDING_NONCE_INFO_V1: &[u8] = b"outbe/tee/onboarding-nonce/v1";

pub fn encrypt_onboarding_artifact_v1(
    sender_offer_secret: &[u8; 32],
    context: DcapOnboardingContextV1,
    group_signature: &[u8],
) -> Result<DcapOnboardingArtifactV1, TeeError> {
    let sender = StaticSecret::from(*sender_offer_secret);
    if PublicKey::from(&sender).to_bytes() != context.network_encryption_public {
        return Err(TeeError::EncryptFailed(None));
    }
    let shared = sender.diffie_hellman(&PublicKey::from(context.recipient_x25519));
    let hash = context.context_hash();
    let key = hkdf_sha256(hash.as_slice(), shared.as_bytes(), ONBOARDING_KEY_INFO_V1)?;
    let nonce_okm = hkdf_sha256(hash.as_slice(), shared.as_bytes(), ONBOARDING_NONCE_INFO_V1)?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_okm[..12]);
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, group_signature)?;
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce,
        ciphertext,
    };
    artifact
        .encode_canonical()
        .map_err(|_| TeeError::EncryptFailed(None))?;
    Ok(artifact)
}
