//! Secret ownership and domain-neutral operations on opaque handles.
pub(crate) mod crypto_ops;
mod envelopes;
mod handles;
mod keys;
pub(crate) mod onboarding_cipher;

pub use crypto_ops::{export_sealed_account_keys, SealedAccountKeysExport};
pub use handles::{
    compute_modify_mac, derive_account_keys_handle, verify_modify_mac, AccountKeysHandle,
    OfferKeyHandle, StateKeyHandle,
};
#[cfg(any(test, feature = "test-utils"))]
pub use handles::{slot_decrypt, slot_encrypt};
pub use keys::{
    ResidentOfferMaterial, ResidentTeeKeys, TEE_BLS_IDENTITY_V1_DST, TEE_OFFER_MESSAGE,
};
pub(crate) use onboarding_cipher::{
    decrypt_onboarding_artifact_v1, encrypt_onboarding_artifact_v1,
};

/// Resident operations preserve structured causes without dumping secret-bearing context.
#[derive(Debug, thiserror::Error)]
pub enum TeeCryptoError {
    #[error("resident cryptographic primitive failed")]
    Primitive(#[from] crate::errors::TeeCryptoError),
    #[error("owner signature recovery failed")]
    OwnerSignature(#[source] outbe_primitives::error::PrecompileError),
    #[error("owner signature does not control this account")]
    WrongOwner,
    #[error("invalid resident cryptographic context")]
    InvalidContext,
    #[error("invalid encrypted envelope")]
    InvalidEnvelope,
    #[error("zero TEE BLS scalar")]
    ZeroBlsScalar,
    #[error("onboarding artifact encoding failed")]
    Artifact {
        code: crate::dcap_protocol::DcapRejectCodeV1,
    },
}

pub type Result<T> = std::result::Result<T, TeeCryptoError>;

#[cfg(test)]
mod tests;
