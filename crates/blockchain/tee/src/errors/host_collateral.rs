use super::{HostCollateralError, TransportError};

impl From<HostCollateralError> for TransportError {
    fn from(error: HostCollateralError) -> Self {
        if matches!(
            error,
            HostCollateralError::InvalidNativeQvlManifest(_)
                | HostCollateralError::MissingQvlArtifact
                | HostCollateralError::MissingQvlPath
                | HostCollateralError::MissingQvlSize
                | HostCollateralError::MissingQvlDigest
                | HostCollateralError::SgxQuoteSizeOverflow
                | HostCollateralError::SgxAuthSizeOverflow
                | HostCollateralError::QeAuthSizeOverflow
                | HostCollateralError::CertificationDataSizeOverflow
                | HostCollateralError::ComponentSizeOverflow { .. }
        ) {
            Self::codec(error)
        } else {
            Self::attestation(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HostCollateralError, TransportError};
    use std::error::Error;

    #[test]
    fn host_collateral_keeps_the_typed_source() {
        let codec = TransportError::from(HostCollateralError::MissingQvlArtifact);
        assert_eq!(codec.metric_class(), "codec");
        assert!(!codec.is_node_local());
        assert!(matches!(
            Error::source(&codec).and_then(|cause| cause.downcast_ref::<HostCollateralError>()),
            Some(HostCollateralError::MissingQvlArtifact)
        ));

        let attestation = TransportError::from(HostCollateralError::RequiresX8664Linux);
        assert_eq!(attestation.metric_class(), "attestation");
        assert!(!attestation.is_node_local());
        assert!(matches!(
            Error::source(&attestation)
                .and_then(|cause| cause.downcast_ref::<HostCollateralError>()),
            Some(HostCollateralError::RequiresX8664Linux)
        ));
    }
}
