//! Process identity and network offer material; no persistence or serialization.
use alloy_primitives::B256;
use commonware_codec::Encode as _;
use commonware_cryptography::{
    bls12381::{
        self,
        primitives::group::{Private, Scalar},
    },
    Signer as _,
};
use ring::rand::SecureRandom as _;
use zeroize::Zeroizing;

use super::{
    crypto_ops::{derive, x25519_public},
    OfferKeyHandle, Result, StateKeyHandle, TeeCryptoError,
};

pub const TEE_BLS_IDENTITY_V1_DST: &[u8] = b"OUTBE_TEE_BLS_IDENTITY_V1_XMD:SHA-256";
pub const TEE_OFFER_MESSAGE: &[u8] = b"outbe/tee/offer/v1";

/// Every owned byte secret is zeroizing. Dalek SigningKey's enabled `zeroize`
/// feature clears its seed; Commonware PrivateKey owns Secret bytes and Scalar
/// (both zeroize on drop). No raw secret is exposed outside this crate.
pub struct ResidentTeeKeys {
    pub(crate) noise_private: Zeroizing<[u8; 32]>,
    pub(crate) tribute_offer_secret: Zeroizing<[u8; 32]>,
    pub(crate) dkg_enc_secret: Zeroizing<[u8; 32]>,
    pub(crate) tee_bls_key: bls12381::PrivateKey,
    attestation_signing: ed25519_dalek::SigningKey,
}

impl std::fmt::Debug for ResidentTeeKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResidentTeeKeys([REDACTED])")
    }
}

impl ResidentTeeKeys {
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0; 32]);
        ring::rand::SystemRandom::new()
            .fill(seed.as_mut())
            .map_err(crate::errors::TeeCryptoError::rng_failure)?;
        Self::new_with_identity_seed(seed)
    }

    pub(crate) fn new_with_identity_seed(seed: Zeroizing<[u8; 32]>) -> Result<Self> {
        let noise_private = derive(seed.as_ref(), b"", b"outbe/tee/noise-static/v1")?;
        let tribute_offer_secret = derive(seed.as_ref(), b"", b"outbe/tee/recipient-x25519/v1")?;
        let attestation = derive(seed.as_ref(), b"", b"outbe/tee/attestation-ed25519/v1")?;
        let dkg_enc_secret = derive(seed.as_ref(), b"", b"outbe/tee/dkg-enc/v1")?;
        let bls_seed = derive(seed.as_ref(), b"", b"outbe/tee/dkg-bls-seed/v1")?;
        let mut tee_bls_key = None;
        for counter in 0..=u8::MAX {
            let mut input = Zeroizing::new([0; 33]);
            input[..32].copy_from_slice(bls_seed.as_ref());
            input[32] = counter;
            let scalar = Scalar::map(TEE_BLS_IDENTITY_V1_DST, input.as_ref());
            if scalar != Scalar::from_u64(0) {
                tee_bls_key = Some(bls12381::PrivateKey::from(Private::new(scalar)));
                break;
            }
        }
        Ok(Self {
            noise_private,
            tribute_offer_secret,
            dkg_enc_secret,
            tee_bls_key: tee_bls_key.ok_or(TeeCryptoError::ZeroBlsScalar)?,
            attestation_signing: ed25519_dalek::SigningKey::from_bytes(&attestation),
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn from_seed(seed: [u8; 32]) -> Result<Self> {
        Self::new_with_identity_seed(Zeroizing::new(seed))
    }

    pub fn noise_public(&self) -> [u8; 32] {
        x25519_public(&self.noise_private)
    }
    pub fn tribute_offer_public(&self) -> [u8; 32] {
        x25519_public(&self.tribute_offer_secret)
    }
    pub fn dkg_enc_public(&self) -> [u8; 32] {
        x25519_public(&self.dkg_enc_secret)
    }
    pub fn attestation_pub(&self) -> [u8; 32] {
        self.attestation_signing.verifying_key().to_bytes()
    }
    pub fn tee_bls_public_bytes(&self) -> Vec<u8> {
        self.tee_bls_key.public_key().encode().to_vec()
    }
    /// Compatibility probe for deterministic test identities, not a production signing API.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn sign_domain_attestation(&self, preimage: &[u8]) -> [u8; 64] {
        self.sign_attestation(preimage)
    }
    pub(crate) fn sign_attestation(&self, preimage: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer as _;
        self.attestation_signing.sign(preimage).to_bytes()
    }
}

pub struct ResidentOfferMaterial {
    pub(crate) secret: OfferKeyHandle,
    pub(crate) public: [u8; 32],
    pub(crate) group_sig: Zeroizing<Vec<u8>>,
    pub(crate) key_epoch: u64,
    pub(crate) tribute_offer_epoch: u64,
}

impl std::fmt::Debug for ResidentOfferMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResidentOfferMaterial([REDACTED])")
    }
}

impl ResidentOfferMaterial {
    /// Import existing sidecar material into opaque RAM handles; removed with the sidecar.
    pub fn from_legacy_parts(
        secret: Zeroizing<[u8; 32]>,
        group_sig: Zeroizing<Vec<u8>>,
        key_epoch: u64,
        tribute_offer_epoch: u64,
    ) -> Self {
        let public = x25519_public(&secret);
        Self {
            secret: OfferKeyHandle::from_legacy_secret(secret),
            public,
            group_sig,
            key_epoch,
            tribute_offer_epoch,
        }
    }

    pub(crate) fn from_group_signature(
        group_sig: Zeroizing<Vec<u8>>,
        chain_id: B256,
        key_epoch: u64,
        tribute_offer_epoch: u64,
    ) -> Result<Self> {
        let mut info = b"outbe/tribute/v1/".to_vec();
        info.extend_from_slice(tribute_offer_epoch.to_string().as_bytes());
        let seed = derive(chain_id.as_slice(), &group_sig, &info)?;
        let secret = derive(seed.as_ref(), b"", b"outbe/tribute/offer-x25519/v1")?;
        let public = x25519_public(&secret);
        Ok(Self {
            secret: OfferKeyHandle(StateKeyHandle(secret)),
            public,
            group_sig,
            key_epoch,
            tribute_offer_epoch,
        })
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }
    pub fn key_epoch(&self) -> u64 {
        self.key_epoch
    }
    pub fn tribute_offer_epoch(&self) -> u64 {
        self.tribute_offer_epoch
    }
    pub fn offer_key_handle(&self) -> &OfferKeyHandle {
        &self.secret
    }
    pub fn derive_state_key_handle(
        &self,
        state_info_label: &[u8],
        chain_id: B256,
        epoch: u64,
    ) -> Result<StateKeyHandle> {
        let mut info = state_info_label.to_vec();
        info.extend_from_slice(epoch.to_string().as_bytes());
        Ok(StateKeyHandle(derive(
            chain_id.as_slice(),
            &self.group_sig,
            &info,
        )?))
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn for_test(
        group_sig: Vec<u8>,
        chain_id: B256,
        key_epoch: u64,
        tribute_offer_epoch: u64,
    ) -> Result<Self> {
        Self::from_group_signature(
            Zeroizing::new(group_sig),
            chain_id,
            key_epoch,
            tribute_offer_epoch,
        )
    }
}
