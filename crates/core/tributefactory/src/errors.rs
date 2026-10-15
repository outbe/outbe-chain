use std::fmt::Display;

use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

/// Errors that the Tribute factory precompile returns.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(clippy::large_enum_variant)]
pub enum TributeFactoryError {
    #[error("TEE not configured")]
    TeeNotConfigured,

    /// Unused error variant. Offer decryption rejections use `EnclaveRejected`.
    /// Transport faults return `PrecompileError::EnclaveUnavailable` because healthy validators can execute the transaction.
    #[error("decryption failed: {0}")]
    DecryptionFailed(String),

    /// The enclave processed the offer and returned `Rejected`.
    /// This deterministic result depends on offer content and reverts the call.
    #[error("enclave rejected the offer: {0}")]
    EnclaveRejected(String),

    #[error("zkProof is required")]
    ZkProofRequired,

    #[error("unknown circuit version {version:?} for L2 chain {chain_id}")]
    UnknownCircuitVersion { chain_id: u32, version: String },

    #[error("ZK circuit does not support the Tribute proof interface")]
    UnsupportedZkCircuit,

    #[error("malformed zkProof: {0}")]
    MalformedZkProof(String),

    #[error("ZK public input mismatch: {field}")]
    ZkPublicInputMismatch { field: &'static str },

    #[error("ZK proof verification failed")]
    InvalidZkProof,

    #[error("worldwide_day {worldwide_day} is not a valid YYYYMMDD calendar date")]
    InvalidWorldwideDay { worldwide_day: WorldwideDay },

    #[error("worldwide_day {worldwide_day} is not in OFFERING status (status={status})")]
    WorldwideDayNotOffering {
        worldwide_day: WorldwideDay,
        status: u8,
    },

    #[error("tribute already exists for this combination of parameters")]
    TributeAlreadyExists,

    #[error("enclave tribute identity does not match Poseidon(owner, worldwide_day)")]
    InvalidCanonicalIdentity,

    #[error("SU hash already used")]
    SuHashAlreadyUsed,

    #[error("invalid SU hash hex: {hash}")]
    InvalidSuHashHex { hash: String },

    #[error("SU hash must be 32 bytes, got {length}")]
    InvalidSuHashLength { length: usize },

    #[error("amount must be positive")]
    AmountMustBePositive,

    #[error("nominal price is zero: no VWAP or S-curve data available for worldwide_day {worldwide_day}")]
    NominalPriceUnavailable { worldwide_day: WorldwideDay },

    #[error("nominal amount overflow")]
    NominalAmountOverflow,

    #[error("invalid wallet address: {address}")]
    InvalidWalletAddress { address: String },

    #[error("invalid SRA address: {address}")]
    InvalidSraAddress { address: String },

    #[error("issuance currency {issuance_currency} not registered in oracle")]
    IssuanceCurrencyNotRegistered { issuance_currency: u16 },

    #[error("Invalid currency code {currency}")]
    InvalidCurrency { currency: u16 },

    #[error("wallet_addresses must be specified when sra_addresses is provided")]
    WalletAddressesRequiredWhenSraProvided,

    #[error("sra_addresses must be specified when wallet_addresses is provided")]
    SraAddressesRequiredWhenWalletProvided,

    #[error("invalid wallet_address at index {index}: {address}")]
    InvalidWalletAddressAtIndex { index: usize, address: String },

    #[error("invalid sra_address at index {index}: {address}")]
    InvalidSraAddressAtIndex { index: usize, address: String },

    #[error("base amount has too many decimal places (max 18)")]
    BaseAmountTooManyDecimals,

    #[error("invalid base amount format")]
    InvalidBaseAmountFormat,

    #[error("invalid atto amount format")]
    InvalidAttoAmountFormat,

    #[error("tee_sidecar_unavailable")]
    TeeSidecarUnavailable,

    #[error("tee_sidecar_unavailable: {0}")]
    TeeSidecarUnavailableWithReason(String),

    #[error("enclave ProcessTributeOfferBatch error: {0}")]
    EnclaveBatchError(String),

    #[error("unexpected enclave response: {0:?}")]
    UnexpectedEnclaveResponse(outbe_tee::protocol::EnclaveResponse),

    #[error("tee_enclave_nondeterminism: inputs_canonical_hash mismatch")]
    InputsCanonicalHashMismatch,

    #[error("tee_offer_attestation_invalid: {0}")]
    OfferAttestationInvalid(String),

    #[error("tee_enclave_nondeterminism: result count mismatch: expected {expected}, got {got}")]
    ResultCountMismatch { expected: usize, got: usize },

    #[error("tee_enclave_nondeterminism: effective reference price mismatch")]
    EffectiveReferencePriceMismatch,

    #[error("encrypted Tribute offer enclave: {0}")]
    EncryptedOfferEnclaveError(String),

    #[error("unexpected encrypted Tribute offer response")]
    UnexpectedEncryptedOfferResponse,

    #[error("tee_enclave_nondeterminism: encrypted offer input or count mismatch")]
    EncryptedOfferInputOrCountMismatch,

    #[error("rejected encrypted offer carries a Tribute")]
    RejectedEncryptedOfferCarriesTribute,

    #[error("created encrypted offer has no Tribute")]
    CreatedEncryptedOfferMissingTribute,

    #[error("tee_enclave_nondeterminism: encrypted Tribute context mismatch")]
    EncryptedTributeContextMismatch,

    #[error("enclave returned an empty tribute offer result")]
    EmptyTributeOfferResult,

    #[error("created Tribute result has no canonical body")]
    CreatedTributeMissingBody,

    #[error("encrypted offer returned a legacy body")]
    EncryptedOfferReturnedLegacyBody,

    #[error("TributeFactory block timestamp exceeds u64")]
    TimestampExceedsU64,

    #[error("ZK verifier unavailable: zk verification backend failed: {0}")]
    ZkVerifierUnavailable(String),

    #[error("{0}")]
    FatalMessage(String),
}

impl TributeFactoryError {
    pub(crate) fn identity_hash(
        error: outbe_compressed_entities::CommitmentError,
    ) -> PrecompileError {
        match error {
            outbe_compressed_entities::CommitmentError::CryptoBackend(error) => {
                PrecompileError::CryptoBackendUnavailable(error.to_string())
            }
            error => Self::fatal(error).into(),
        }
    }

    pub fn malformed_zk_proof(err: impl Display) -> Self {
        Self::MalformedZkProof(err.to_string())
    }

    pub fn sidecar_unavailable(err: impl Display) -> Self {
        Self::TeeSidecarUnavailableWithReason(err.to_string())
    }

    pub fn offer_attestation_invalid(err: impl Display) -> Self {
        Self::OfferAttestationInvalid(err.to_string())
    }

    pub fn zk_verifier_unavailable(err: impl Display) -> Self {
        Self::ZkVerifierUnavailable(err.to_string())
    }

    pub fn fatal(err: impl Display) -> Self {
        Self::FatalMessage(err.to_string())
    }
}

impl From<TributeFactoryError> for PrecompileError {
    fn from(value: TributeFactoryError) -> Self {
        match value {
            enclave @ (TributeFactoryError::TeeSidecarUnavailable
            | TributeFactoryError::TeeSidecarUnavailableWithReason(_)
            | TributeFactoryError::EnclaveBatchError(_)
            | TributeFactoryError::EncryptedOfferEnclaveError(_)
            | TributeFactoryError::UnexpectedEnclaveResponse(_)
            | TributeFactoryError::UnexpectedEncryptedOfferResponse
            | TributeFactoryError::InputsCanonicalHashMismatch
            | TributeFactoryError::OfferAttestationInvalid(_)
            | TributeFactoryError::ResultCountMismatch { .. }
            | TributeFactoryError::EffectiveReferencePriceMismatch
            | TributeFactoryError::EncryptedOfferInputOrCountMismatch
            | TributeFactoryError::EncryptedTributeContextMismatch) => {
                PrecompileError::EnclaveUnavailable(enclave.to_string())
            }
            TributeFactoryError::ZkVerifierUnavailable(_)
            | TributeFactoryError::TimestampExceedsU64
            | TributeFactoryError::RejectedEncryptedOfferCarriesTribute
            | TributeFactoryError::CreatedEncryptedOfferMissingTribute
            | TributeFactoryError::EmptyTributeOfferResult
            | TributeFactoryError::CreatedTributeMissingBody
            | TributeFactoryError::EncryptedOfferReturnedLegacyBody
            | TributeFactoryError::FatalMessage(_) => PrecompileError::Revert(value.to_string()),
            other => PrecompileError::Revert(other.to_string()),
        }
    }
}

/// Errors that occur when the host connects to the enclave client.
#[derive(Debug, Error)]
pub enum EnclaveInitError {
    #[error("TEE enclave endpoint is not valid UTF-8")]
    InvalidEndpointUtf8,

    #[error("TEE enclave connect at {socket} failed: {source}")]
    ConnectFailed {
        socket: String,
        #[source]
        source: outbe_tee::TransportError,
    },

    #[error("unexpected enclave handshake response: {0:?}")]
    UnexpectedHandshakeResponse(outbe_tee::protocol::EnclaveResponse),

    #[error("enclave GetPublicKeys failed: {0}")]
    GetPublicKeysFailed(#[source] outbe_tee::TransportError),

    #[error(transparent)]
    InstallFailed(#[from] outbe_tee::InstallError),
}

pub type TributeFactoryResult<T> = std::result::Result<T, TributeFactoryError>;
