//! Typed error definitions for OCOMP and TEE genesis generation commands.

/// Represents errors during OCOMP genesis, bindings, and successor generation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OcompGenesisError {
    /// Indicates that the successor activation height is not above genesis.
    #[error("successor activation height must be above genesis")]
    SuccessorActivationHeightBelowGenesis,
    /// Indicates that the OCOMP bundle catalog directory does not exist.
    #[error("OCOMP bundle catalog directory does not exist: {path}")]
    BundleCatalogDirNotFound {
        /// Missing directory path.
        path: String,
    },
    /// Indicates that the predecessor OCOMP bundle is not canonical.
    #[error("predecessor OCOMP bundle is not canonical")]
    NonCanonicalPredecessorBundle,
    /// Indicates that the successor OCOMP bundle is not canonical.
    #[error("successor OCOMP bundle is not canonical")]
    NonCanonicalSuccessorBundle,
    /// Indicates that the version-1 predecessor does not match the genesis OCOMP authority.
    #[error("version-1 predecessor does not match the genesis OCOMP authority")]
    Version1PredecessorMismatch,
    /// Indicates an invalid Update protocol version string.
    #[error("invalid Update protocol version: {details}")]
    InvalidUpdateProtocolVersion {
        /// Version parse failure details.
        details: String,
    },
    /// Indicates that building the Update payload failed.
    #[error("build Update payload: {details}")]
    BuildUpdatePayload {
        /// Payload construction failure details.
        details: String,
    },
    /// Indicates that `--input` and `--output` paths are identical.
    #[error("--input and --output must differ")]
    IdenticalInputAndOutput,
    /// Indicates that the input genesis `config` field is not a JSON object.
    #[error("input genesis config must be a JSON object")]
    InputGenesisConfigNotObject,
    /// Indicates that the input genesis already contains an OCOMP or Metadosis layout extension.
    #[error("input genesis already contains an OCOMP or Metadosis layout extension")]
    InputGenesisAlreadyHasExtension,
    /// Indicates that the base input genesis already contains OCOMP extensions.
    #[error("input genesis must not already contain OCOMP extensions")]
    BaseIdentityAlreadyHasOcompExtensions,
    /// Indicates that the validators manifest is not a JSON array.
    #[error("validators manifest must be a JSON array")]
    ValidatorsManifestNotArray,
    /// Indicates that the consensus validator bound overflows `usize`.
    #[error("consensus validator bound does not fit usize")]
    ConsensusValidatorBoundOverflow,
    /// Indicates that the validators manifest is empty.
    #[error("validators manifest must not be empty")]
    ValidatorsManifestEmpty,
    /// Indicates that the validators manifest exceeds the consensus validator bound.
    #[error("validators manifest contains {count} entries, above consensus bound {max}")]
    ValidatorsManifestExceedsBound {
        /// Manifest validator count.
        count: usize,
        /// Maximum allowed validators.
        max: usize,
    },
    /// Indicates that a validator entry lacks an `address` field.
    #[error("validator-{index} has no address")]
    ValidatorMissingAddress {
        /// Validator index.
        index: usize,
    },
    /// Indicates that a validator entry lacks a `public_key` field.
    #[error("validator-{index} has no public_key")]
    ValidatorMissingPublicKey {
        /// Validator index.
        index: usize,
    },
    /// Indicates that a validator's BLS public key is not 48 bytes.
    #[error("validator-{index} public key must be 48 bytes")]
    ValidatorInvalidPublicKeyLength {
        /// Validator index.
        index: usize,
    },
    /// Indicates that an OCOMP registration directory entry name is not valid UTF-8.
    #[error("OCOMP registration directory entry is not UTF-8")]
    RegistrationEntryNotUtf8,
    /// Indicates an extra OCOMP registration directory beyond the validator count.
    #[error("extra OCOMP registration directory {name} for {count} validators")]
    ExtraRegistrationDir {
        /// Directory entry name.
        name: String,
        /// Expected validator count.
        count: usize,
    },
    /// Indicates that an OCOMP registration entry is not a directory.
    #[error("OCOMP registration entry {name} is not a directory")]
    RegistrationEntryNotDir {
        /// Entry name.
        name: String,
    },
    /// Indicates that the founder registration count does not match the validator count.
    #[error(
        "founder registration count {registrations} does not match validator count {validators}"
    )]
    FounderRegistrationCountMismatch {
        /// Loaded registration count.
        registrations: usize,
        /// Expected validator count.
        validators: usize,
    },
    /// Indicates that a validator's OCOMP registration does not match this genesis.
    #[error("validator-{index} OCOMP registration does not match this genesis")]
    ValidatorRegistrationGenesisMismatch {
        /// Validator index.
        index: usize,
    },
    /// Indicates that a validator reuses an OCOMP public key.
    #[error("validator-{index} reuses an OCOMP public key")]
    ValidatorDuplicatePublicKey {
        /// Validator index.
        index: usize,
    },
    /// Indicates that the generated genesis changed the chain ID.
    #[error("generated genesis changed chain ID")]
    GeneratedGenesisChangedChainId,
    /// Indicates that the OCOMP extension changed the genesis hash.
    #[error("OCOMP extension changed genesis hash")]
    OcompExtensionChangedGenesisHash,
    /// Indicates that the node parser returned a different OCOMP install.
    #[error("node parser returned a different OCOMP install")]
    NodeParserInstallMismatch,
    /// Indicates refusal to overwrite an existing output file.
    #[error("refusing to overwrite existing {description}: {path}")]
    OverwriteExistingOutput {
        /// Output file description.
        description: String,
        /// Output file path.
        path: String,
    },
    /// Indicates that the parent directory of an output file does not exist.
    #[error("{description} parent does not exist: {path}")]
    OutputParentNotFound {
        /// Output file description.
        description: String,
        /// Output file path.
        path: String,
    },
    /// Indicates that a path is not valid UTF-8.
    #[error("{description} path is not UTF-8: {path}")]
    PathNotUtf8 {
        /// Path description.
        description: String,
        /// Non-UTF-8 path display string.
        path: String,
    },
}

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
