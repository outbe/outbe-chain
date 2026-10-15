//! Typed errors for host-side Intel DCAP collateral acquisition and parsing.

use thiserror::Error;

use super::TransportError;

/// Errors returned during host-side Intel QVL/QPL collateral acquisition.
#[derive(Debug, Error)]
pub enum HostCollateralError {
    #[error("pinned QVL path contains an interior NUL")]
    PinnedQvlPathInteriorNul,
    #[error("cannot load pinned Intel QVL: {0}")]
    CannotLoadPinnedIntelQvl(String),
    #[error("pinned Intel QVL is missing a collateral symbol: {0}")]
    MissingCollateralSymbol(String),
    #[error("production DCAP collateral acquisition requires x86_64 Linux")]
    RequiresX8664Linux,
    #[error("SGX quote length exceeds the Intel QVL ABI")]
    QuoteLengthExceedsAbi,
    #[error("Intel tee_qv_get_collateral failed with quote3_error_t {status:#06x}")]
    GetCollateralFailed { status: u32 },
    #[error("Intel collateral allocation is smaller than its ABI header")]
    CollateralAllocationSmallerThanHeader,
    #[error("Intel collateral ABI is not supported SGX PCS v3.0 or v3.1")]
    UnsupportedCollateralAbiVersion,
    #[error("invalid native QVL manifest: {0}")]
    InvalidNativeQvlManifest(serde_json::Error),
    #[error("native QVL manifest has no QVL artifact")]
    MissingQvlArtifact,
    #[error("native QVL path is missing")]
    MissingQvlPath,
    #[error("native QVL size is missing")]
    MissingQvlSize,
    #[error("native QVL digest is missing")]
    MissingQvlDigest,
    #[error("pinned Intel QVL path is not absolute")]
    PinnedQvlPathNotAbsolute,
    #[error("cannot read pinned Intel QVL: {0}")]
    CannotReadPinnedIntelQvl(std::io::Error),
    #[error("installed Intel QVL size differs from the project pin")]
    InstalledQvlSizeMismatch,
    #[error("installed Intel QVL digest differs from the project pin")]
    InstalledQvlDigestMismatch,
    #[error("type-5 PCK chain is not canonical PEM plus NUL")]
    NonCanonicalType5PckChain,
    #[error("SGX quote size overflow")]
    SgxQuoteSizeOverflow,
    #[error("quote is not a complete SGX quote v3")]
    IncompleteSgxQuoteV3,
    #[error("SGX authentication size overflow")]
    SgxAuthSizeOverflow,
    #[error("SGX quote has trailing or truncated authentication bytes")]
    SgxQuoteAuthBytesMismatch,
    #[error("QE authentication is truncated")]
    QeAuthTruncated,
    #[error("QE authentication size overflow")]
    QeAuthSizeOverflow,
    #[error("quote certification-data header is truncated")]
    CertificationDataHeaderTruncated,
    #[error("certification-data size overflow")]
    CertificationDataSizeOverflow,
    #[error("quote does not contain exact type-5 PCK certification data")]
    MissingExactType5PckCertificationData,
    #[error("{label} size overflow")]
    ComponentSizeOverflow { label: &'static str },
    #[error("Intel QPL returned invalid {label} length")]
    InvalidQplComponentLength { label: &'static str },
    #[error("{label} is not terminated by exactly one NUL")]
    ComponentNotSingleNulTerminated { label: &'static str },
    #[error("{label} is not UTF-8")]
    ComponentNotUtf8 { label: &'static str },
    #[error("{label} contains odd-length hexadecimal DER")]
    OddLengthHexDer { label: &'static str },
    #[error("{label} is not hexadecimal DER")]
    NotHexDer { label: &'static str },
    #[error("{label} is empty")]
    ComponentEmpty { label: &'static str },
    #[error("{label} uses unsupported collateral ABI")]
    UnsupportedComponentCollateralAbi { label: &'static str },
}

impl From<HostCollateralError> for TransportError {
    fn from(error: HostCollateralError) -> Self {
        match error {
            HostCollateralError::InvalidNativeQvlManifest(_)
            | HostCollateralError::MissingQvlArtifact
            | HostCollateralError::MissingQvlPath
            | HostCollateralError::MissingQvlSize
            | HostCollateralError::MissingQvlDigest
            | HostCollateralError::SgxQuoteSizeOverflow
            | HostCollateralError::SgxAuthSizeOverflow
            | HostCollateralError::QeAuthSizeOverflow
            | HostCollateralError::CertificationDataSizeOverflow
            | HostCollateralError::ComponentSizeOverflow { .. } => Self::Codec(error.to_string()),
            _ => Self::Attestation(error.to_string()),
        }
    }
}
