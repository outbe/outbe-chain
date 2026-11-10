//! Errors for genesis-fixed TEE attestation authority.

use core::fmt::Display;
use std::path::Path;
use std::sync::Arc;

/// Errors that occur when the node checks TEE genesis and ChainSpec data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeeAttestationActivationError {
    #[error("seeded genesis path is not UTF-8")]
    SeededGenesisPathNotUtf8,
    #[error("parse seeded genesis ChainSpec: {0}")]
    ParseSeededGenesisChainSpec(String),
    #[error("read seeded genesis {path}: {reason}")]
    ReadSeededGenesis { path: String, reason: String },
    #[error("parse seeded genesis JSON: {0}")]
    ParseSeededGenesisJson(String),
    #[error("seeded genesis has no alloc object")]
    MissingSeededGenesisAlloc,
    #[error("seeded genesis has no ValidatorSet allocation")]
    MissingValidatorSetAllocation,
    #[error("seeded genesis ValidatorSet has no storage")]
    MissingValidatorSetStorage,
    #[error("seeded genesis validator count does not fit usize")]
    ValidatorCountOverflow,
    #[error("seeded genesis validator count is outside 1..=256")]
    ValidatorCountOutOfRange,
    #[error("seeded genesis contains an invalid validator address")]
    InvalidValidatorAddress,
    #[error("seeded genesis committee member is not ACTIVE with a BLS share")]
    CommitteeMemberNotActiveWithBlsShare,
    #[error("seeded genesis consensus key high word is not right-padded")]
    ConsensusKeyHighWordNotRightPadded,
    #[error("seeded genesis contains an invalid MinPk consensus key")]
    InvalidMinPkConsensusKey,
    #[error("seeded genesis contains duplicate consensus keys")]
    DuplicateConsensusKeys,
    #[error("seeded genesis lacks ValidatorSet {label}")]
    MissingValidatorSetWord { label: String },
    #[error("decode ValidatorSet {label}: {reason}")]
    DecodeValidatorSetWord { label: String, reason: String },
    #[error("seeded genesis ValidatorSet {label} exceeds one word")]
    ValidatorSetWordExceedsOneWord { label: String },
    #[error("invalid active TEE policy at block {height}: {reason}")]
    InvalidActiveTeePolicy { height: u64, reason: String },
    #[error("DCAP genesis path is not UTF-8")]
    DcapGenesisPathNotUtf8,
    #[error("parse DCAP genesis ChainSpec: {0}")]
    ParseDcapGenesisChainSpec(String),
    #[error("{0}")]
    InvalidChainSpecActivation(String),
    #[error("DCAP release ChainSpec requires DcapRequired policy")]
    RequiresDcapRequiredPolicy,
    #[error("encode DCAP TEE policy: {0}")]
    EncodeDcapTeePolicy(String),
    #[error("hash DCAP TEE policy: {0}")]
    HashDcapTeePolicy(String),
    #[error("encode DCAP TEE policy schedule: {0}")]
    EncodeDcapTeePolicySchedule(String),
    #[error(
        "active DCAP policy must admit Platform UpToDate | SWHardeningNeeded | ConfigurationAndSWHardeningNeeded and QE UpToDate"
    )]
    InvalidDcapTcbStatuses,
    #[error("active DCAP policy does not exactly bind the signed bundle measurement")]
    ReleaseMeasurementMismatch,
    #[error("derive canonical initial DCAP policy: {0}")]
    DeriveCanonicalInitialDcapPolicy(String),
    #[error(
        "active DCAP policy does not equal the canonical initial policy for the signed release"
    )]
    NonCanonicalInitialDcapPolicy,
    #[error("{field}.activationHeight must be {expected}")]
    InvalidActivationHeight { field: &'static str, expected: u64 },
    #[error("invalid canonical TEE policy schedule: {0}")]
    InvalidCanonicalPolicySchedule(String),
    #[error("cannot hash TEE policy schedule: {0}")]
    CannotHashPolicySchedule(String),
    #[error("TEE policy schedule hash does not match canonical bytes")]
    PolicyScheduleHashMismatch,
    #[error("cannot derive normative resource schedule: {0}")]
    CannotDeriveNormativeResourceSchedule(String),
    #[error("TEE manifest does not bind the normative resource schedule")]
    ManifestResourceScheduleMismatch,
    #[error("TEE schedule has no block-1 policy: {0}")]
    MissingBlock1Policy(String),
    #[error(
        "DcapRequired requires devnet, testnet, or mainnet chain ID ({devnet}, {testnet}, or {mainnet})"
    )]
    DcapRequiredChainIdDisallowed {
        devnet: u64,
        testnet: u64,
        mainnet: u64,
    },
    #[error("GramineDirectDev requires devnet or testnet chain ID ({devnet} or {testnet})")]
    GramineDirectDevChainIdDisallowed { devnet: u64, testnet: u64 },
    #[error("initial TEE policy does not bind the manifest resource schedule")]
    InitialPolicyResourceScheduleMismatch,
    #[error("TEE policy schedule does not match ChainSpec identity")]
    PolicyScheduleChainSpecIdentityMismatch,
    #[error("genesis TEE policy schedule must contain exactly one initial policy")]
    GenesisPolicyScheduleEntryCount,
    #[error("TEE policy schedule must be 0x-prefixed hexadecimal")]
    PolicyScheduleMissingHexPrefix,
    #[error("TEE policy schedule cap overflow")]
    PolicyScheduleCapOverflow,
    #[error("TEE policy schedule exceeds {max_bytes} bytes")]
    PolicyScheduleExceedsMaxBytes { max_bytes: usize },
    #[error("TEE policy schedule is not canonical hexadecimal bytes")]
    PolicyScheduleNotCanonicalHex,
    #[error("decode TEE policy schedule: {0}")]
    DecodePolicyScheduleHex(String),
    #[error("TEE attestation ChainSpec authority is not bound")]
    UnboundAuthority,
    #[error("genesis config is missing required {field}")]
    MissingGenesisConfigField { field: &'static str },
    #[error("invalid genesis config {field}: {reason}")]
    InvalidGenesisConfigField { field: &'static str, reason: String },
}

impl TeeAttestationActivationError {
    /// Checks whether the rendered error message contains the given text.
    #[must_use]
    pub fn contains(&self, pattern: &str) -> bool {
        self.to_string().contains(pattern)
    }

    pub(crate) fn parse_seeded_chain_spec(error: impl Display) -> Self {
        Self::ParseSeededGenesisChainSpec(error.to_string())
    }

    pub(crate) fn read_seeded_genesis(path: &Path, error: impl Display) -> Self {
        Self::ReadSeededGenesis {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }

    pub(crate) fn parse_seeded_json(error: impl Display) -> Self {
        Self::ParseSeededGenesisJson(error.to_string())
    }

    pub(crate) fn decode_validator_set_word(label: &str, error: impl Display) -> Self {
        Self::DecodeValidatorSetWord {
            label: label.to_owned(),
            reason: error.to_string(),
        }
    }

    pub(crate) fn invalid_active_policy(height: u64, error: impl Display) -> Self {
        Self::InvalidActiveTeePolicy {
            height,
            reason: error.to_string(),
        }
    }

    pub(crate) fn parse_dcap_chain_spec(error: impl Display) -> Self {
        Self::ParseDcapGenesisChainSpec(error.to_string())
    }

    pub(crate) fn invalid_chain_spec_activation(error: impl Display) -> Self {
        Self::InvalidChainSpecActivation(error.to_string())
    }

    pub(crate) fn invalid_genesis_config_field(field: &'static str, error: impl Display) -> Self {
        Self::InvalidGenesisConfigField {
            field,
            reason: error.to_string(),
        }
    }

    pub(crate) fn encode_dcap_policy(error: impl Display) -> Self {
        Self::EncodeDcapTeePolicy(error.to_string())
    }

    pub(crate) fn hash_dcap_policy(error: impl Display) -> Self {
        Self::HashDcapTeePolicy(error.to_string())
    }

    pub(crate) fn encode_dcap_policy_schedule(error: impl Display) -> Self {
        Self::EncodeDcapTeePolicySchedule(error.to_string())
    }

    pub(crate) fn derive_canonical_initial_dcap_policy(error: impl Display) -> Self {
        Self::DeriveCanonicalInitialDcapPolicy(error.to_string())
    }

    pub(crate) fn invalid_canonical_policy_schedule(error: impl Display) -> Self {
        Self::InvalidCanonicalPolicySchedule(error.to_string())
    }

    pub(crate) fn cannot_hash_policy_schedule(error: impl Display) -> Self {
        Self::CannotHashPolicySchedule(error.to_string())
    }

    pub(crate) fn cannot_derive_normative_resource_schedule(error: impl Display) -> Self {
        Self::CannotDeriveNormativeResourceSchedule(error.to_string())
    }

    pub(crate) fn missing_block1_policy(error: impl Display) -> Self {
        Self::MissingBlock1Policy(error.to_string())
    }

    pub(crate) fn decode_policy_schedule_hex(error: impl Display) -> Self {
        Self::DecodePolicyScheduleHex(error.to_string())
    }
}

impl From<TeeAttestationActivationError> for Arc<str> {
    fn from(error: TeeAttestationActivationError) -> Self {
        Arc::from(error.to_string())
    }
}

impl From<TeeAttestationActivationError> for String {
    fn from(error: TeeAttestationActivationError) -> Self {
        error.to_string()
    }
}
