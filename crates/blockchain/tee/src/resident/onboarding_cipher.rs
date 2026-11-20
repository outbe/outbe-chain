//! Purpose-bound deterministic onboarding, restricted to resident key owners.
use super::{
    crypto_ops::{derive, open, seal, shared},
    ResidentOfferMaterial, ResidentTeeKeys, Result, TeeCryptoError,
};
use crate::dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1};
use alloy_primitives::B256;
use zeroize::Zeroizing;

const KEY_INFO: &[u8] = b"outbe/tee/onboarding-key/v1";
const NONCE_INFO: &[u8] = b"outbe/tee/onboarding-nonce/v1";

pub(crate) fn derive_tribute_offer_secret_from_group_sig(
    group_sig: Zeroizing<Vec<u8>>,
    chain_id: B256,
    key_epoch: u64,
    tribute_offer_epoch: u64,
) -> Result<ResidentOfferMaterial> {
    ResidentOfferMaterial::from_group_signature(group_sig, chain_id, key_epoch, tribute_offer_epoch)
}

/// There is no caller-selected plaintext: only the resident group signature is sealed.
pub(crate) fn encrypt_onboarding_artifact_v1(
    material: &ResidentOfferMaterial,
    context: DcapOnboardingContextV1,
) -> Result<DcapOnboardingArtifactV1> {
    if material.public != context.network_encryption_public {
        return Err(TeeCryptoError::InvalidContext);
    }
    let shared = shared(&material.secret.0 .0, &context.recipient_x25519)?;
    let hash = context.context_hash();
    let key = derive(hash.as_slice(), shared.as_ref(), KEY_INFO)?;
    // The onboarding nonce uses context_hash as salt, not the ECDH value.
    let nonce_bytes = derive(hash.as_slice(), shared.as_ref(), NONCE_INFO)?;
    let mut expected = [0; 12];
    expected.copy_from_slice(&nonce_bytes[..12]);
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: expected,
        ciphertext: seal(&key, &expected, &material.group_sig)?,
    };
    artifact
        .encode_canonical()
        .map_err(|code| TeeCryptoError::Artifact { code })?;
    Ok(artifact)
}

/// Decrypt and activate in RAM. The decrypted root never leaves `outbe-tee`.
pub(crate) fn decrypt_onboarding_artifact_v1(
    keys: &ResidentTeeKeys,
    artifact: &DcapOnboardingArtifactV1,
) -> Result<ResidentOfferMaterial> {
    if keys.tribute_offer_public() != artifact.context.recipient_x25519 {
        return Err(TeeCryptoError::InvalidContext);
    }
    let shared = shared(
        &keys.tribute_offer_secret,
        &artifact.context.network_encryption_public,
    )?;
    let hash = artifact.context.context_hash();
    let key = derive(hash.as_slice(), shared.as_ref(), KEY_INFO)?;
    let nonce_bytes = derive(hash.as_slice(), shared.as_ref(), NONCE_INFO)?;
    let mut expected = [0; 12];
    expected.copy_from_slice(&nonce_bytes[..12]);
    if artifact.nonce != expected {
        return Err(TeeCryptoError::InvalidContext);
    }
    let group_sig = open(&key, &expected, &artifact.ciphertext)?;
    let material = derive_tribute_offer_secret_from_group_sig(
        group_sig,
        B256::from(artifact.context.chain_id),
        artifact.context.key_epoch,
        artifact.context.network_encryption_epoch,
    )?;
    if material.public != artifact.context.network_encryption_public {
        return Err(TeeCryptoError::InvalidContext);
    }
    Ok(material)
}

#[cfg(test)]
mod tests {
    use super::{KEY_INFO, NONCE_INFO};

    const DKG_SHARE_INFO: &[u8] = b"outbe/tee/dkg-share/v1";
    const REGISTRY_NONCE_INFO: &[u8] = b"outbe/tee/registry-seal-nonce/v1";

    #[test]
    fn onboarding_domains_are_distinct_from_existing_share_domains() {
        assert_ne!(KEY_INFO, NONCE_INFO);
        assert_ne!(KEY_INFO, DKG_SHARE_INFO);
        assert_ne!(KEY_INFO, REGISTRY_NONCE_INFO);
        assert_ne!(NONCE_INFO, DKG_SHARE_INFO);
        assert_ne!(NONCE_INFO, REGISTRY_NONCE_INFO);
    }
}
