//! Typed error definitions for enclave transport sessions, offer keys, and dispatch.

use std::fmt;
use std::path::Path;

use outbe_tee::TransportError;

/// Error from enclave transport offer-key persistence and request dispatch.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportDispatchError {
    /// Reading the sealed permanent offer key at boot failed.
    #[error("read sealed permanent offer key {path}: {detail}; no recovery or fallback exists")]
    ReadSealedPermanentOfferKey { path: String, detail: String },
    /// Unsealing the permanent offer key at boot failed.
    #[error(
        "unseal permanent offer key {path} failed: {detail}; \
         this identity has lost its key and no recovery or fallback exists"
    )]
    UnsealPermanentOfferKeyFailed { path: String, detail: String },
    /// Generating the random nonce for offer-key sealing failed.
    #[error("offer-key seal nonce RNG failed")]
    OfferKeySealNonceRngFailed,
    /// Sealing the permanent offer key failed.
    #[error("seal permanent offer key: {0}")]
    SealPermanentOfferKey(String),
    /// Reading an existing concurrently written sealed offer key failed.
    #[error("read concurrent sealed offer key: {0}")]
    ReadConcurrentSealedOfferKey(String),
    /// Unsealing an existing concurrently written sealed offer key failed.
    #[error("verify existing sealed offer key: {0}")]
    VerifyExistingSealedOfferKey(String),
    /// The existing sealed offer key differs from the newly derived key.
    #[error("existing sealed offer key differs from onboarding result")]
    ExistingSealedOfferKeyDiffers,
    /// Writing the sealed offer key file failed.
    #[error("write sealed offer key {path}: {detail}")]
    WriteSealedOfferKey { path: String, detail: String },
    /// The in-memory offer key slot holds a different key after persistence.
    #[error("offer key divergence after durable persistence")]
    OfferKeyDivergenceAfterPersistence,
    /// Computing the REPORT_DATA offset in a synthetic quote underflowed.
    #[error("synthetic quote REPORT_DATA offset underflow")]
    SyntheticQuoteOffsetUnderflow,
    /// The generated quote REPORT_DATA does not match the requested intent.
    #[error("generated DCAP quote REPORT_DATA does not match requested intent")]
    GeneratedQuoteReportDataMismatch,
    /// `DkgOpen` was called without initialized state.
    #[error("DkgOpen requires initialized state")]
    DkgOpenRequiresInitializedState,
    /// `DkgOpen` was called without a committed network binding.
    #[error("DkgOpen requires a committed network binding")]
    DkgOpenRequiresCommittedNetworkBinding,
    /// `DkgFinalizeTributeOffer` was called without initialized state.
    #[error("DkgFinalizeTributeOffer requires initialized state")]
    DkgFinalizeRequiresInitializedState,
    /// `DkgFinalizeTributeOffer` was called without a committed network binding.
    #[error("DkgFinalizeTributeOffer requires a committed network binding")]
    DkgFinalizeRequiresCommittedNetworkBinding,
    /// Founding DKG persistence requires initialized production state.
    #[error("founding DKG persistence requires initialized production state")]
    FoundingDkgPersistenceRequiresInitState,
    /// Founding DKG persistence requires an initialization manifest.
    #[error("founding DKG persistence requires an initialization manifest")]
    FoundingDkgPersistenceRequiresManifest,
    /// Founding DKG in production requires durable sealed storage.
    #[error("founding DKG requires durable production sealed storage")]
    FoundingDkgRequiresDurableStorage,
    /// The recovered offer key differs from the resident offer key.
    #[error("offer key divergence: recovered key differs from the resident offer key")]
    OfferKeyDivergenceFromResident,
    /// No resident network key is installed in the enclave.
    #[error("no resident network key")]
    NoResidentNetworkKey,
    /// The resident chain ID does not fit in u64.
    #[error("resident chain id exceeds u64")]
    ResidentChainIdExceedsU64,
    /// The Lysis request chain ID does not match the resident chain.
    #[error("Lysis chain mismatch")]
    LysisChainMismatch,
    /// Hashing the Lysis request inputs failed.
    #[error("invalid NOD prepare inputs")]
    InvalidLysisInputs,
    /// Building the Lysis result attestation preimage failed.
    #[error("invalid NOD prepare result")]
    InvalidLysisResult,
    /// The NOD mine request chain ID does not match the resident chain.
    #[error("NOD mine chain mismatch")]
    NodMineChainMismatch,
    /// Deriving the Gratis state key failed.
    #[error("Gratis key unavailable")]
    GratisKeyUnavailable,
    /// Deriving the Fidelity state key failed.
    #[error("Fidelity key unavailable")]
    FidelityKeyUnavailable,
    /// Deriving the Tribute state key failed.
    #[error("Tribute key unavailable")]
    TributeKeyUnavailable,
    /// Deriving the NOD state key failed.
    #[error("NOD key unavailable")]
    NodKeyUnavailable,
    /// Hashing the NOD mint request inputs failed.
    #[error("invalid NOD mint inputs")]
    InvalidNodMintInputs,
    /// Building the NOD rejection attestation preimage failed.
    #[error("invalid NOD rejection result")]
    InvalidNodRejectionResult,
    /// Applying the encrypted NOD mint operation failed.
    #[error("encrypted NOD mint failed")]
    EncryptedNodMintFailed,
    /// Building the NOD mine result attestation preimage failed.
    #[error("invalid NOD mine result")]
    InvalidNodMineResult,
    /// The NOD read request chain ID does not match the resident chain.
    #[error("NOD read chain mismatch")]
    NodReadChainMismatch,
    /// Decrypting the encrypted NOD failed.
    #[error("invalid encrypted NOD")]
    InvalidEncryptedNod,
    /// Hashing the NOD read request inputs failed.
    #[error("invalid NOD read inputs")]
    InvalidNodReadInputs,
    /// The NOD test fixture chain ID does not match the resident chain.
    #[error("NOD fixture chain mismatch")]
    NodFixtureChainMismatch,
    /// Hashing the NOD test fixture inputs failed.
    #[error("invalid NOD fixture inputs")]
    InvalidNodFixtureInputs,
    /// Encrypting the NOD test fixture failed.
    #[error("invalid NOD fixture")]
    InvalidNodFixture,
    /// Building the NOD test fixture attestation preimage failed.
    #[error("invalid NOD fixture result")]
    InvalidNodFixtureResult,
    /// The NOD Tribute encryption chain ID does not match the resident chain.
    #[error("NOD Tribute encryption chain mismatch")]
    NodTributeEncryptChainMismatch,
    /// Hashing the NOD Tribute encryption inputs failed.
    #[error("invalid NOD Tribute encryption inputs")]
    InvalidNodTributeEncryptInputs,
    /// Encrypting the NOD from the source Tribute failed.
    #[error("invalid NOD Tribute encryption")]
    InvalidNodTributeEncrypt,
    /// Building the NOD Tribute encryption attestation preimage failed.
    #[error("invalid NOD Tribute encryption result")]
    InvalidNodTributeEncryptResult,
    /// The NOD transfer total size or payload is out of bounds.
    #[error("invalid NOD transfer bounds")]
    InvalidNodTransferBounds,
    /// The NOD transfer chunk size or offset is out of bounds.
    #[error("invalid NOD transfer chunk bounds")]
    InvalidNodTransferChunkBounds,
    /// Decoding the buffered NOD transfer request failed.
    #[error("NOD transfer request encoding")]
    NodTransferRequestEncoding,
    /// The buffered NOD transfer command is not permitted on the transfer path.
    #[error("NOD transfer command not permitted")]
    NodTransferCommandNotPermitted,
    /// Onboarding artifact ingest requires an initialization manifest.
    #[error("onboarding ingest requires an initialization manifest")]
    OnboardingIngestRequiresManifest,
    /// Decoding the upgrade key export artifact failed.
    #[error("invalid upgrade export context")]
    InvalidUpgradeExportContext,
    /// The upgrade export source enclave is not initialized.
    #[error("source is not initialized")]
    SourceNotInitialized,
    /// The upgrade export lost its verified network authorization.
    #[error("upgrade export lost its verified network authorization")]
    UpgradeExportLostAuthorization,
    /// The upgrade export source key is unavailable.
    #[error("source key is unavailable")]
    SourceKeyUnavailable,
    /// The upgrade export source key does not match the finalized commitment.
    #[error("source key does not match the finalized network commitment")]
    SourceKeyMismatchCommitment,
    /// Canonical encoding of the exported onboarding artifact failed.
    #[error("artifact encoding: {0}")]
    ArtifactEncoding(String),
    /// DirectDev onboarding ingest requires production initialization.
    #[error("GramineDirectDev onboarding requires production initialization")]
    GramineDirectDevRequiresProdInit,
    /// DirectDev onboarding ingest requires an initialization manifest.
    #[error("GramineDirectDev onboarding requires an initialization manifest")]
    GramineDirectDevRequiresManifest,
    /// DirectDev onboarding is forbidden by the initialized network.
    #[error("GramineDirectDev onboarding is forbidden by the initialized network")]
    GramineDirectDevForbiddenByNetwork,
    /// DirectDev onboarding requires durable sealed storage.
    #[error("GramineDirectDev onboarding requires durable production sealed storage")]
    GramineDirectDevRequiresDurableStorage,
    /// Decoding the registration intent failed.
    #[error("registration intent is not canonical: {0}")]
    RegistrationIntentNotCanonical(String),
    /// Generating a transition quote requires the permanent offer key.
    #[error("transition quote generation requires the permanent offer key")]
    TransitionQuoteRequiresOfferKey,
    /// The enclave is not initialized.
    #[error("enclave is not initialized")]
    EnclaveNotInitialized,
    /// Generating transition evidence requires an initialized enclave.
    #[error("transition requires initialized enclave")]
    TransitionRequiresInitializedEnclave,
    /// DirectDev transition evidence is disabled for the current enclave mode.
    #[error("DirectDev transition is disabled for this enclave mode")]
    DirectDevTransitionDisabled,
    /// The registration intent is not a DirectDev measurement transition.
    #[error("expected a DirectDev measurement transition")]
    ExpectedDirectDevMeasurementTransition,
    /// Generating transition evidence requires the permanent offer key.
    #[error("transition requires the permanent offer key")]
    TransitionRequiresPermanentOfferKey,
    /// The DirectDev registration intent does not bind this enclave identity.
    #[error("GramineDirectDev intent does not bind this enclave identity")]
    GramineDirectDevIntentIdentityMismatch,
    /// Remote session admission requires an initialized enclave.
    #[error("remote admission requires initialization")]
    RemoteAdmissionRequiresInitialization,
    /// Remote session retirement requires an initialized enclave.
    #[error("remote retirement requires initialized enclave")]
    RemoteRetirementRequiresInitializedEnclave,
    /// Encoding the encrypted Tribute offer results failed.
    #[error("cannot encode encrypted Tribute results")]
    CannotEncodeEncryptedTributeResults,
    /// An encrypted Tribute record chain ID differs from the resident chain.
    #[error("Tribute chain differs from resident chain")]
    TributeChainDiffersFromResident,
    /// Decrypting an encrypted Tribute record failed.
    #[error("Tribute decryption failed")]
    TributeDecryptionFailed,
    /// Encoding the Tribute read inputs failed.
    #[error("cannot encode Tribute read inputs")]
    CannotEncodeTributeReadInputs,
    /// Encoding the Tribute read results failed.
    #[error("cannot encode Tribute read results")]
    CannotEncodeTributeReadResults,
    /// A Tribute day record chain ID differs from the resident chain.
    #[error("Tribute day chain differs from resident chain")]
    TributeDayChainDiffersFromResident,
    /// Applying a Tribute day operation failed.
    #[error("Tribute day operation failed")]
    TributeDayOperationFailed,
    /// Encoding the Tribute day operation inputs failed.
    #[error("cannot encode Tribute day inputs")]
    CannotEncodeTributeDayInputs,
    /// Encoding the Tribute day operation result failed.
    #[error("cannot encode Tribute day result")]
    CannotEncodeTributeDayResult,
    /// Decrypting a Tribute day record failed.
    #[error("Tribute day decryption failed")]
    TributeDayDecryptionFailed,
    /// Encoding the Tribute day read inputs failed.
    #[error("cannot encode Tribute day read inputs")]
    CannotEncodeTributeDayReadInputs,
    /// An underlying primitive or crypto operation failed.
    #[error("{0}")]
    Detail(String),
}

impl TransportDispatchError {
    /// Wrap a permanent offer key read error at boot.
    pub fn read_sealed_permanent_offer_key(path: &Path, err: impl fmt::Display) -> Self {
        Self::ReadSealedPermanentOfferKey {
            path: path.display().to_string(),
            detail: err.to_string(),
        }
    }

    /// Wrap a permanent offer key unseal error at boot.
    pub fn unseal_permanent_offer_key_failed(path: &Path, err: impl fmt::Display) -> Self {
        Self::UnsealPermanentOfferKeyFailed {
            path: path.display().to_string(),
            detail: err.to_string(),
        }
    }

    /// Wrap a permanent offer key seal error.
    pub fn seal_permanent_offer_key(err: impl fmt::Display) -> Self {
        Self::SealPermanentOfferKey(err.to_string())
    }

    /// Wrap a concurrent sealed offer key read error.
    pub fn read_concurrent_sealed_offer_key(err: impl fmt::Display) -> Self {
        Self::ReadConcurrentSealedOfferKey(err.to_string())
    }

    /// Wrap a concurrent sealed offer key verification error.
    pub fn verify_existing_sealed_offer_key(err: impl fmt::Display) -> Self {
        Self::VerifyExistingSealedOfferKey(err.to_string())
    }

    /// Wrap a sealed offer key write error.
    pub fn write_sealed_offer_key(path: &Path, err: impl fmt::Display) -> Self {
        Self::WriteSealedOfferKey {
            path: path.display().to_string(),
            detail: err.to_string(),
        }
    }

    /// Wrap an onboarding artifact encoding error.
    pub fn artifact_encoding(err: impl fmt::Debug) -> Self {
        Self::ArtifactEncoding(format!("{err:?}"))
    }

    /// Wrap a registration intent decode error.
    pub fn registration_intent_not_canonical(err: impl fmt::Display) -> Self {
        Self::RegistrationIntentNotCanonical(err.to_string())
    }

    /// Wrap a generic display error.
    pub fn detail(err: impl fmt::Display) -> Self {
        Self::Detail(err.to_string())
    }
}

/// Convert a displayable error into `TransportError::Noise`.
pub fn transport_noise(err: impl fmt::Display) -> TransportError {
    TransportError::Noise(err.to_string())
}

/// Convert a debuggable error into `TransportError::Noise`.
pub fn transport_noise_debug(err: impl fmt::Debug) -> TransportError {
    TransportError::Noise(format!("{err:?}"))
}

/// Convert a displayable error into `TransportError::Handshake`.
pub fn transport_handshake(err: impl fmt::Display) -> TransportError {
    TransportError::Handshake(err.to_string())
}

/// Handshake errors when establishing a remote session read deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RemoteSessionHandshakeError {
    /// The host system clock is earlier than the Unix epoch.
    #[error("system time precedes Unix epoch")]
    SystemTimeBeforeUnixEpoch,
    /// The remote session lease has already expired.
    #[error("remote session lease expired")]
    LeaseExpired,
}

impl From<RemoteSessionHandshakeError> for TransportError {
    fn from(err: RemoteSessionHandshakeError) -> Self {
        TransportError::Handshake(err.to_string())
    }
}
