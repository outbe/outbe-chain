use outbe_ocomp_protocol::ProtocolError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisTestSupportError {
    #[error("capacity fixture exceeds MAX_RETAINED_WWDS")]
    CapacityFixtureExceedsMaxRetainedWwds,

    #[error("capacity fixture contains an invalid WorldwideDay")]
    CapacityFixtureInvalidWorldwideDay,

    #[error("{error}")]
    Protocol {
        #[from]
        error: ProtocolError,
    },

    #[error("seeded OCOMP Registry authority does not read back")]
    SeededOcompRegistryAuthorityMismatch,

    #[error("test founder validator index exceeds u8")]
    FounderValidatorIndexExceedsU8,

    #[error("test founder validator identity failed: {error}")]
    FounderValidatorIdentityFailed { error: ProtocolError },

    #[error("test founder OCOMP key is not 33 bytes")]
    FounderOcompKeyNot33Bytes,

    #[error("test founder PoP digest failed: {error}")]
    FounderPopDigestFailed { error: ProtocolError },

    #[error("activation fixture parent partition binding mismatch")]
    ActivationFixtureParentPartitionBindingMismatch,

    #[error("activation fixture does not seal its synthetic parent")]
    ActivationFixtureDoesNotSealSyntheticParent,

    #[error("fixture OCOMP intent key is invalid: {error}")]
    FixtureOcompIntentKeyInvalid { error: ProtocolError },

    #[error("Metadosis active WWD cap {max_active_wwds} reached before inserting {wwd}")]
    ActiveWwdCapReached {
        max_active_wwds: usize,
        wwd: WorldwideDay,
    },
}

impl From<MetadosisTestSupportError> for PrecompileError {
    fn from(value: MetadosisTestSupportError) -> Self {
        PrecompileError::Fatal(value.to_string())
    }
}
