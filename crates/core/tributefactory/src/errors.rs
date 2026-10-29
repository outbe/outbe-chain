use std::fmt::Display;

use crate::precompile::ITributeFactory;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
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

    #[error("worldwide_day {worldwide_day} is not in OFFERING status")]
    WorldwideDayNotOffering { worldwide_day: WorldwideDay },

    #[error("tribute already exists for this combination of parameters")]
    TributeAlreadyExists,

    #[error("enclave tribute identity does not match Poseidon(owner, worldwide_day)")]
    InvalidCanonicalIdentity,

    #[error("SU hash already used")]
    SuHashAlreadyUsed,

    #[error("SU hash list is empty")]
    EmptySuHashList,

    #[error("invalid SU hash hex: {hash}")]
    InvalidSuHashHex { hash: String },

    #[error("SU hash must be 32 bytes, got {length}")]
    InvalidSuHashLength { length: usize },

    #[error("amount must be positive")]
    AmountMustBePositive,

    #[error("currency price is zero: no VWAP or S-curve data available for worldwide_day {worldwide_day}")]
    CurrencyPriceUnavailable { worldwide_day: WorldwideDay },

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
    UnexpectedEnclaveResponse(Box<outbe_tee::protocol::EnclaveResponse>),

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

    #[error("ZK verifier unavailable: zk verification backend failed: {0}")]
    ZkVerifierUnavailable(String),

    #[error(transparent)]
    EntityIdDerivation(#[from] outbe_primitives::wwd_entity_id::EntityIdDerivationError),

    #[error(transparent)]
    CalculationView(#[from] outbe_tee::TransportError),

    #[error("{0}")]
    AttestationPreimageEncode(String),
}

impl TributeFactoryError {
    pub fn attestation_preimage_encode(err: impl Display) -> Self {
        Self::AttestationPreimageEncode(err.to_string())
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
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<TributeFactoryError> for PrecompileError {
    fn from(value: TributeFactoryError) -> Self {
        use ITributeFactory as I;
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
            TributeFactoryError::TeeNotConfigured => revert_with(I::TeeNotConfigured {}),
            TributeFactoryError::EnclaveRejected(value) => {
                revert_with(I::EnclaveRejected { value })
            }
            TributeFactoryError::ZkProofRequired => revert_with(I::ZkProofRequired {}),
            TributeFactoryError::UnknownCircuitVersion { chain_id, version } => {
                revert_with(I::UnknownCircuitVersion {
                    chainId: chain_id,
                    version,
                })
            }
            TributeFactoryError::UnsupportedZkCircuit => revert_with(I::UnsupportedZkCircuit {}),
            TributeFactoryError::MalformedZkProof(value) => {
                revert_with(I::MalformedZkProof { value })
            }
            TributeFactoryError::ZkPublicInputMismatch { field } => {
                revert_with(I::ZkPublicInputMismatch {
                    field: field.to_string(),
                })
            }
            TributeFactoryError::InvalidZkProof => revert_with(I::InvalidZkProof {}),
            TributeFactoryError::InvalidWorldwideDay { worldwide_day } => {
                revert_with(I::InvalidWorldwideDay {
                    worldwideDay: worldwide_day.value(),
                })
            }
            TributeFactoryError::WorldwideDayNotOffering { worldwide_day } => {
                revert_with(I::WorldwideDayNotOffering {
                    worldwideDay: worldwide_day.value(),
                })
            }
            TributeFactoryError::TributeAlreadyExists => revert_with(I::TributeAlreadyExists {}),
            TributeFactoryError::InvalidCanonicalIdentity => {
                revert_with(I::InvalidCanonicalIdentity {})
            }
            TributeFactoryError::SuHashAlreadyUsed => revert_with(I::SuHashAlreadyUsed {}),
            TributeFactoryError::EmptySuHashList => revert_with(I::EmptySuHashList {}),
            TributeFactoryError::InvalidSuHashHex { hash } => {
                revert_with(I::InvalidSuHashHex { hash })
            }
            TributeFactoryError::InvalidSuHashLength { length } => {
                revert_with(I::InvalidSuHashLength {
                    length: u64::try_from(length).unwrap_or(u64::MAX),
                })
            }
            TributeFactoryError::AmountMustBePositive => revert_with(I::AmountMustBePositive {}),
            TributeFactoryError::CurrencyPriceUnavailable { worldwide_day } => {
                revert_with(I::CurrencyPriceUnavailable {
                    worldwideDay: worldwide_day.value(),
                })
            }
            TributeFactoryError::NominalAmountOverflow => revert_with(I::NominalAmountOverflow {}),
            TributeFactoryError::InvalidWalletAddress { address } => {
                revert_with(I::InvalidWalletAddress {
                    walletAddress: address,
                })
            }
            TributeFactoryError::InvalidSraAddress { address } => {
                revert_with(I::InvalidSraAddress {
                    sraAddress: address,
                })
            }
            TributeFactoryError::IssuanceCurrencyNotRegistered { issuance_currency } => {
                revert_with(I::IssuanceCurrencyNotRegistered {
                    issuanceCurrency: issuance_currency,
                })
            }
            TributeFactoryError::InvalidCurrency { currency } => {
                revert_with(I::InvalidCurrency { currency })
            }
            TributeFactoryError::WalletAddressesRequiredWhenSraProvided => {
                revert_with(I::WalletAddressesRequiredWhenSraProvided {})
            }
            TributeFactoryError::SraAddressesRequiredWhenWalletProvided => {
                revert_with(I::SraAddressesRequiredWhenWalletProvided {})
            }
            TributeFactoryError::InvalidWalletAddressAtIndex { index, address } => {
                revert_with(I::InvalidWalletAddressAtIndex {
                    index: u64::try_from(index).unwrap_or(u64::MAX),
                    walletAddress: address,
                })
            }
            TributeFactoryError::InvalidSraAddressAtIndex { index, address } => {
                revert_with(I::InvalidSraAddressAtIndex {
                    index: u64::try_from(index).unwrap_or(u64::MAX),
                    sraAddress: address,
                })
            }
            TributeFactoryError::BaseAmountTooManyDecimals => {
                revert_with(I::BaseAmountTooManyDecimals {})
            }
            TributeFactoryError::InvalidBaseAmountFormat => {
                revert_with(I::InvalidBaseAmountFormat {})
            }
            TributeFactoryError::InvalidAttoAmountFormat => {
                revert_with(I::InvalidAttoAmountFormat {})
            }
            TributeFactoryError::RejectedEncryptedOfferCarriesTribute => {
                revert_with(I::RejectedEncryptedOfferCarriesTribute {})
            }
            TributeFactoryError::CreatedEncryptedOfferMissingTribute => {
                revert_with(I::CreatedEncryptedOfferMissingTribute {})
            }
            TributeFactoryError::EmptyTributeOfferResult => {
                revert_with(I::EmptyTributeOfferResult {})
            }
            TributeFactoryError::CreatedTributeMissingBody => {
                revert_with(I::CreatedTributeMissingBody {})
            }
            TributeFactoryError::EncryptedOfferReturnedLegacyBody => {
                revert_with(I::EncryptedOfferReturnedLegacyBody {})
            }
            TributeFactoryError::ZkVerifierUnavailable(value) => {
                revert_with(I::ZkVerifierUnavailable { value })
            }
            TributeFactoryError::EntityIdDerivation(err) => revert_with(I::EntityIdDerivation {
                value: err.to_string(),
            }),
            TributeFactoryError::CalculationView(err) => revert_with(I::CalculationView {
                value: err.to_string(),
            }),
            TributeFactoryError::AttestationPreimageEncode(value) => {
                revert_with(I::AttestationPreimageEncode { value })
            }
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
