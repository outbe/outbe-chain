//! Errors for genesis-fixed TEE attestation authority.

use std::path::Path;
use std::sync::Arc;

/// Typed cause retained on an activation failure.
#[derive(Clone)]
pub struct ActivationCause {
    source: Arc<dyn std::error::Error + Send + Sync>,
}

impl ActivationCause {
    pub(crate) fn new(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            source: Arc::new(error),
        }
    }
}

/// Uses pointer-identity equality (`Arc::ptr_eq`) so cloned
/// `TeeAttestationChainSpecStateV1` values compare equal while distinct cause
/// allocations compare unequal; outer error variants and fields still compare
/// structurally.
impl PartialEq for ActivationCause {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
    }
}

impl Eq for ActivationCause {}

impl std::fmt::Debug for ActivationCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.source, formatter)
    }
}

impl std::fmt::Display for ActivationCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.source, formatter)
    }
}

impl std::error::Error for ActivationCause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Errors that occur when the node checks TEE genesis and ChainSpec data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeeAttestationActivationError {
    #[error("seeded genesis path is not UTF-8")]
    SeededGenesisPathNotUtf8,
    #[error("parse seeded genesis ChainSpec: {0}")]
    ParseSeededGenesisChainSpec(#[source] ActivationCause),
    #[error("read seeded genesis {path}: {reason}")]
    ReadSeededGenesis {
        path: String,
        #[source]
        reason: ActivationCause,
    },
    #[error("parse seeded genesis JSON: {0}")]
    ParseSeededGenesisJson(#[source] ActivationCause),
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
    DecodeValidatorSetWord {
        label: String,
        #[source]
        reason: ActivationCause,
    },
    #[error("seeded genesis ValidatorSet {label} exceeds one word")]
    ValidatorSetWordExceedsOneWord { label: String },
    #[error("invalid active TEE policy at block {height}: {reason}")]
    InvalidActiveTeePolicy {
        height: u64,
        #[source]
        reason: ActivationCause,
    },
    #[error("DCAP genesis path is not UTF-8")]
    DcapGenesisPathNotUtf8,
    #[error("parse DCAP genesis ChainSpec: {0}")]
    ParseDcapGenesisChainSpec(#[source] ActivationCause),
    #[error("{0}")]
    InvalidChainSpecActivation(#[source] ActivationCause),
    #[error("DCAP release ChainSpec requires DcapRequired policy")]
    RequiresDcapRequiredPolicy,
    #[error("encode DCAP TEE policy: {0}")]
    EncodeDcapTeePolicy(#[source] ActivationCause),
    #[error("hash DCAP TEE policy: {0}")]
    HashDcapTeePolicy(#[source] ActivationCause),
    #[error("encode DCAP TEE policy schedule: {0}")]
    EncodeDcapTeePolicySchedule(#[source] ActivationCause),
    #[error(
        "active DCAP policy must admit Platform UpToDate | SWHardeningNeeded | ConfigurationAndSWHardeningNeeded and QE UpToDate"
    )]
    InvalidDcapTcbStatuses,
    #[error("active DCAP policy does not exactly bind the signed bundle measurement")]
    ReleaseMeasurementMismatch,
    #[error("derive canonical initial DCAP policy: {0}")]
    DeriveCanonicalInitialDcapPolicy(#[source] ActivationCause),
    #[error(
        "active DCAP policy does not equal the canonical initial policy for the signed release"
    )]
    NonCanonicalInitialDcapPolicy,
    #[error("{field}.activationHeight must be {expected}")]
    InvalidActivationHeight { field: &'static str, expected: u64 },
    #[error("invalid canonical TEE policy schedule: {0}")]
    InvalidCanonicalPolicySchedule(#[source] ActivationCause),
    #[error("cannot hash TEE policy schedule: {0}")]
    CannotHashPolicySchedule(#[source] ActivationCause),
    #[error("TEE policy schedule hash does not match canonical bytes")]
    PolicyScheduleHashMismatch,
    #[error("cannot derive normative resource schedule: {0}")]
    CannotDeriveNormativeResourceSchedule(#[source] ActivationCause),
    #[error("TEE manifest does not bind the normative resource schedule")]
    ManifestResourceScheduleMismatch,
    #[error("TEE schedule has no block-1 policy: {0}")]
    MissingBlock1Policy(#[source] ActivationCause),
    #[error(
        "DcapRequired requires devnet, testnet, or mainnet chain ID ({devnet}, {testnet}, or {mainnet})"
    )]
    DcapRequiredChainIdDisallowed {
        devnet: u64,
        testnet: u64,
        mainnet: u64,
    },
    #[error("NoAttestationDev requires devnet or testnet chain ID ({devnet} or {testnet})")]
    NoAttestationDevChainIdDisallowed { devnet: u64, testnet: u64 },
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
    DecodePolicyScheduleHex(#[source] ActivationCause),
    #[error("TEE attestation ChainSpec authority is not bound")]
    UnboundAuthority,
    #[error("genesis config is missing required {field}")]
    MissingGenesisConfigField { field: &'static str },
    #[error("invalid genesis config {field}: {reason}")]
    InvalidGenesisConfigField {
        field: &'static str,
        #[source]
        reason: ActivationCause,
    },
}

impl TeeAttestationActivationError {
    pub(crate) fn parse_seeded_chain_spec(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::ParseSeededGenesisChainSpec(ActivationCause::new(error))
    }

    pub(crate) fn read_seeded_genesis(
        path: &Path,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::ReadSeededGenesis {
            path: format!("{}", path.display()),
            reason: ActivationCause::new(error),
        }
    }

    pub(crate) fn parse_seeded_json(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::ParseSeededGenesisJson(ActivationCause::new(error))
    }

    pub(crate) fn decode_validator_set_word(
        label: &str,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::DecodeValidatorSetWord {
            label: label.to_owned(),
            reason: ActivationCause::new(error),
        }
    }

    pub(crate) fn invalid_active_policy(
        height: u64,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::InvalidActiveTeePolicy {
            height,
            reason: ActivationCause::new(error),
        }
    }

    pub(crate) fn parse_dcap_chain_spec(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::ParseDcapGenesisChainSpec(ActivationCause::new(error))
    }

    pub(crate) fn invalid_chain_spec_activation(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::InvalidChainSpecActivation(ActivationCause::new(error))
    }

    pub(crate) fn invalid_genesis_config_field(
        field: &'static str,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::InvalidGenesisConfigField {
            field,
            reason: ActivationCause::new(error),
        }
    }

    pub(crate) fn encode_dcap_policy(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::EncodeDcapTeePolicy(ActivationCause::new(error))
    }

    pub(crate) fn hash_dcap_policy(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::HashDcapTeePolicy(ActivationCause::new(error))
    }

    pub(crate) fn encode_dcap_policy_schedule(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::EncodeDcapTeePolicySchedule(ActivationCause::new(error))
    }

    pub(crate) fn derive_canonical_initial_dcap_policy(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::DeriveCanonicalInitialDcapPolicy(ActivationCause::new(error))
    }

    pub(crate) fn invalid_canonical_policy_schedule(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::InvalidCanonicalPolicySchedule(ActivationCause::new(error))
    }

    pub(crate) fn cannot_hash_policy_schedule(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::CannotHashPolicySchedule(ActivationCause::new(error))
    }

    pub(crate) fn cannot_derive_normative_resource_schedule(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::CannotDeriveNormativeResourceSchedule(ActivationCause::new(error))
    }

    pub(crate) fn missing_block1_policy(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::MissingBlock1Policy(ActivationCause::new(error))
    }

    pub(crate) fn decode_policy_schedule_hex(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::DecodePolicyScheduleHex(ActivationCause::new(error))
    }
}
