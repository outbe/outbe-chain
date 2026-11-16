use super::{HostCollateralError, TransportError};

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
