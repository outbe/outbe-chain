//! Typed error definitions for TEE genesis generation commands.

/// Represents errors during TEE genesis and network descriptor generation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeGenesisError {
    /// Indicates a ChainSpec binding validation failure.
    #[error("{details}")]
    ChainSpecBinding {
        /// Binding failure details.
        details: String,
    },
    /// Indicates that genesis is missing its fixed TEE policy schedule.
    #[error("genesis is missing its fixed TEE policy schedule")]
    MissingPolicySchedule,
    /// Indicates a genesis policy decoding error.
    #[error("genesis policy: {details}")]
    GenesisPolicy {
        /// Policy error details.
        details: String,
    },
    /// Indicates that the TEE policy does not match the actual genesis hash.
    #[error("TEE policy does not match the actual genesis hash")]
    PolicyGenesisHashMismatch,
    /// Indicates that genesis has no initial policy.
    #[error("genesis has no initial policy")]
    MissingInitialPolicy,
    /// Indicates a network descriptor encoding error.
    #[error("network descriptor: {details}")]
    NetworkDescriptor {
        /// Descriptor error details.
        details: String,
    },
    /// Indicates that `--input` and `--output` paths are identical.
    #[error("--input and --output must differ")]
    IdenticalInputAndOutput,
    /// Indicates refusal to overwrite an existing genesis output file.
    #[error("refusing to overwrite existing genesis output {path}")]
    OverwriteExistingOutput {
        /// Existing output path.
        path: String,
    },
    /// Indicates that the genesis output parent directory does not exist.
    #[error("genesis output parent does not exist: {path}")]
    OutputParentNotFound {
        /// Output path whose parent is missing.
        path: String,
    },
    /// Indicates a missing required DCAP argument.
    #[error("DcapRequired requires {name}")]
    MissingDcapArgument {
        /// Missing CLI argument name.
        name: String,
    },
    /// Indicates that DCAP measurement arguments were passed in `GramineDirectDev` mode.
    #[error(
        "GramineDirectDev uses fixed non-hardware projections; DCAP measurement arguments are forbidden"
    )]
    DcapArgsForbiddenInDevMode,
    /// Indicates that the input genesis `config` field is not a JSON object.
    #[error("input genesis config must be a JSON object")]
    InputGenesisConfigNotObject,
    /// Indicates that the input genesis already contains `teeAttestationV1`.
    #[error("input genesis already contains {field}; refusing an implicit policy replacement")]
    InputGenesisAlreadyHasField {
        /// Duplicate field name.
        field: &'static str,
    },
    /// Indicates that the generated genesis changed the chain ID.
    #[error("generated genesis changed chain ID")]
    GeneratedGenesisChangedChainId,
    /// Indicates that `teeAttestationV1` unexpectedly changed the genesis header hash.
    #[error("teeAttestationV1 unexpectedly changed the genesis header hash")]
    TeeFieldChangedGenesisHash,
    /// Indicates a mismatch in the generated genesis attestation mode.
    #[error("generated genesis attestation mode mismatch")]
    GeneratedGenesisModeMismatch,
    /// Indicates that a hex argument is not 32 bytes.
    #[error("expected exactly 32 hexadecimal bytes")]
    InvalidHex32Bytes,
    /// Indicates that a path is not valid UTF-8.
    #[error("{label} path is not valid UTF-8: {path}")]
    PathNotUtf8 {
        /// Path label.
        label: String,
        /// Non-UTF-8 path display string.
        path: String,
    },
}
