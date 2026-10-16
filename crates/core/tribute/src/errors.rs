//! Domain error types for the Tribute crate.

use outbe_primitives::error::PrecompileError;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use thiserror::Error;

/// Represents domain errors in the Tribute precompile and runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TributeError {
    #[error("tribute not found")]
    TributeNotFound,

    #[error("tribute already exists")]
    TributeAlreadyExists,

    #[error("invalid owner")]
    InvalidOwner,

    #[error("settlement amount must be positive")]
    SettlementAmountMustBePositive,

    #[error("worldwide day is sealed")]
    WorldwideDaySealed,

    #[error("tribute pre-admission inputs are sealed")]
    PreAdmissionSealed,

    #[error("Tribute OCOMP profile is not initialized")]
    OcompProfileNotReady,

    #[error("owner balance overflow")]
    OwnerBalanceOverflow,

    #[error("tribute query exceeds the runtime result limit")]
    QueryLimitExceeded,

    #[error("certified Tribute input differs from the sealed generation")]
    CertifiedInputDiffersFromSealedGeneration,

    #[error("certified Tribute generation is already retired")]
    CertifiedGenerationAlreadyRetired,

    #[error("sealed Tribute state contradicts the authenticated generation")]
    SealedProjectionContradictsAuthenticatedCeGeneration,

    #[error("certified Tribute pre-admission record is absent")]
    CertifiedPreAdmissionRecordAbsent,

    #[error("certified Tribute source generation changed during activation")]
    CertifiedSourceGenerationChangedDuringActivation,

    #[error("certified Tribute activation binding mismatch")]
    CertifiedActivationBindingMismatch,

    #[error("invalid certified Tribute retirement aggregate")]
    InvalidCertifiedRetirementAggregate,

    #[error("invalid certified Tribute protocol value: {0}")]
    InvalidCertifiedProtocolValue(String),

    #[error("invalid encrypted Tribute day identity")]
    InvalidEncryptedDayIdentity,

    #[error("Tribute day enclave: Tribute day response scope mismatch")]
    DayResponseScopeMismatch,

    #[error("invalid encrypted Tribute encoding")]
    InvalidEncryptedEncoding,

    #[error("Tribute chain identity mismatch")]
    ChainIdentityMismatch,

    #[error("Tribute total supply overflow during issuance")]
    TotalSupplyOverflowDuringIssuance,

    #[error("{0}")]
    EntityIdDerivation(String),

    #[error("{0}")]
    CalculationView(String),

    #[error("populated OCOMP Tribute input requires certified retirement")]
    PopulatedOcompInputRequiresCertifiedRetirement,

    #[error("Lysis input count/nominal does not match sealed Tribute DayTotals")]
    LysisInputMismatch,

    #[error("Tribute total supply underflow during Lysis")]
    TotalSupplyUnderflowDuringLysis,

    #[error("MissedOffering requires a sealed empty Tribute partition")]
    MissedOfferingRequiresSealedEmptyPartition,

    #[error("Tribute WWD is not completed and empty")]
    WorldwideDayNotCompletedAndEmpty,

    #[error("Tribute total supply underflow during burn")]
    TotalSupplyUnderflowDuringBurn,

    #[error("Tribute OCOMP profile requires empty live state")]
    OcompProfileRequiresEmptyLiveState,

    #[error("day metadata write differs from encrypted amount")]
    DayMetadataDiffersFromEncryptedAmount,

    #[error("sealed day metadata write differs from encrypted amount")]
    SealedDayMetadataDiffersFromEncryptedAmount,

    #[error("Tribute canonical body length exceeds u64")]
    CanonicalBodyLengthOverflow,

    #[error("Tribute canonical body byte total overflow")]
    CanonicalBodyByteTotalOverflow,

    #[error("Tribute distinct reference currency count overflow")]
    DistinctReferenceCurrencyCountOverflow,

    #[error("Tribute reference currency membership refcount overflow")]
    ReferenceCurrencyRefcountOverflow,

    #[error("Tribute canonical body byte total underflow")]
    CanonicalBodyByteTotalUnderflow,

    #[error("Tribute reference currency membership refcount underflow")]
    ReferenceCurrencyRefcountUnderflow,

    #[error("Tribute distinct reference currency count underflow")]
    DistinctReferenceCurrencyCountUnderflow,

    #[error("Tribute day count overflow or underflow")]
    DayCountOverflowOrUnderflow,

    #[error("Tribute private amount read failed: {0}")]
    PrivateAmountReadFailed(String),

    #[error("Tribute canonical identity mismatch: expected {expected}, found {found}")]
    CanonicalIdentityMismatch {
        expected: WwdEntityId,
        found: WwdEntityId,
    },

    #[error("Tribute total supply underflow during CapacityForfeiture")]
    TotalSupplyUnderflowDuringCapacityForfeiture,

    #[error("Tribute retirement outcome contradicts authenticated partition root")]
    RetirementOutcomeContradictsAuthenticatedRoot,

    #[error("Tribute source generation overflow during CapacityForfeiture")]
    SourceGenerationOverflowDuringCapacityForfeiture,

    #[error("CapacityForfeiture requires a fresh-profile sealed Tribute partition")]
    CapacityForfeitureRequiresFreshSealedPartition,

    #[error("populated Tribute aggregate has no authenticated parent partition")]
    PopulatedAggregateMissingAuthenticatedParent,

    #[error("CapacityForfeiture requires unretired Tribute generation zero")]
    CapacityForfeitureRequiresUnretiredGenerationZero,

    #[error("sealed Tribute pre-admission differs from authenticated aggregate")]
    SealedPreAdmissionDiffersFromAuthenticatedAggregate,

    #[error(transparent)]
    CanonicalBody(#[from] outbe_primitives::entity_codec::CanonicalBodyError),

    #[error(transparent)]
    DayAmountCodec(#[from] postcard::Error),

    #[error("Tribute day enclave: {0}")]
    DayEnclave(#[source] outbe_tee::TransportError),
}

impl TributeError {
    /// Converts a certified protocol failure into a precompile error.
    pub(crate) fn invalid_certified_protocol_value(
        error: impl std::fmt::Display,
    ) -> PrecompileError {
        Self::InvalidCertifiedProtocolValue(error.to_string()).into()
    }

    /// Converts an entity-ID derivation failure into a precompile error.
    pub(crate) fn entity_id_derivation(error: impl std::fmt::Display) -> PrecompileError {
        Self::EntityIdDerivation(error.to_string()).into()
    }

    /// Converts a private amount read failure into a precompile error.
    pub(crate) fn private_amount_read_failed(error: outbe_tee::TransportError) -> PrecompileError {
        let message = Self::PrivateAmountReadFailed(error.to_string()).to_string();
        if error.is_node_local() {
            PrecompileError::EnclaveUnavailable(message)
        } else {
            PrecompileError::Revert(message)
        }
    }
}

impl From<TributeError> for PrecompileError {
    fn from(value: TributeError) -> Self {
        let message = value.to_string();
        match value {
            TributeError::DayEnclave(_) => PrecompileError::EnclaveUnavailable(message),
            TributeError::InvalidEncryptedDayIdentity
            | TributeError::InvalidEncryptedEncoding
            | TributeError::ChainIdentityMismatch
            | TributeError::TotalSupplyOverflowDuringIssuance
            | TributeError::LysisInputMismatch
            | TributeError::TotalSupplyUnderflowDuringLysis
            | TributeError::MissedOfferingRequiresSealedEmptyPartition
            | TributeError::TotalSupplyUnderflowDuringBurn
            | TributeError::DayMetadataDiffersFromEncryptedAmount
            | TributeError::SealedDayMetadataDiffersFromEncryptedAmount
            | TributeError::CanonicalBodyLengthOverflow
            | TributeError::CanonicalBodyByteTotalOverflow
            | TributeError::DistinctReferenceCurrencyCountOverflow
            | TributeError::ReferenceCurrencyRefcountOverflow
            | TributeError::CanonicalBodyByteTotalUnderflow
            | TributeError::ReferenceCurrencyRefcountUnderflow
            | TributeError::DistinctReferenceCurrencyCountUnderflow
            | TributeError::DayCountOverflowOrUnderflow
            | TributeError::TotalSupplyUnderflowDuringCapacityForfeiture
            | TributeError::RetirementOutcomeContradictsAuthenticatedRoot
            | TributeError::SourceGenerationOverflowDuringCapacityForfeiture
            | TributeError::CapacityForfeitureRequiresFreshSealedPartition
            | TributeError::PopulatedAggregateMissingAuthenticatedParent
            | TributeError::CapacityForfeitureRequiresUnretiredGenerationZero
            | TributeError::SealedPreAdmissionDiffersFromAuthenticatedAggregate
            | TributeError::CanonicalBody(_)
            | TributeError::DayAmountCodec(_) => PrecompileError::BodyReadCorruption(message),
            _ => PrecompileError::Revert(message),
        }
    }
}

/// Wraps a test-enclave decryption error in an enclave transport error.
#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn enclave_transport_error(error: impl std::fmt::Display) -> outbe_tee::TransportError {
    outbe_tee::TransportError::EnclaveError(error.to_string())
}

pub type TributeResult<T> = std::result::Result<T, TributeError>;

#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn body_read_corruption(error: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::BodyReadCorruption(error.to_string())
}
