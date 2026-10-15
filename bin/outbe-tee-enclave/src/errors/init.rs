//! Typed error definitions for enclave initialization, onboarding upload, sealing, and startup.

use std::fmt;

use outbe_primitives::tee_attestation_v1::AttestationMode;

/// Error from write-once enclave initialization and remote session admission.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InitError {
    /// System time precedes the Unix epoch.
    #[error("system time precedes Unix epoch")]
    SystemTimePrecedesEpoch,
    /// A remote session lease has expired.
    #[error("remote session lease expired")]
    RemoteSessionLeaseExpired,
    /// Initialization challenge was requested in development mode.
    #[error("initialization challenge is unavailable in development mode")]
    ChallengeUnavailableInDev,
    /// The enclave is already initialized.
    #[error("enclave is already initialized")]
    AlreadyInitialized,
    /// Production initialization was requested in development mode.
    #[error("production initialization is unavailable in development mode")]
    ProdInitUnavailableInDev,
    /// The initialization manifest failed canonical decoding.
    #[error("initialization manifest is not canonical: {0}")]
    ManifestNotCanonical(String),
    /// The node signature is not 65 bytes.
    #[error("initialization node signature must be 65 bytes")]
    NodeSignatureLen,
    /// The node signature failed verification.
    #[error("initialization node signature is invalid")]
    NodeSignatureInvalid,
    /// The initialization state mutex is poisoned.
    #[error("initialization state lock is poisoned")]
    StateLockPoisoned,
    /// Production initialization was attempted without sealed storage.
    #[error("production initialization requires sealed storage")]
    RequiresSealedStorage,
    /// The enclave has not been initialized yet.
    #[error("enclave is not initialized")]
    NotInitialized,
    /// The registration intent failed canonical decoding.
    #[error("registration intent is not canonical: {0}")]
    IntentNotCanonical(String),
    /// Production quote generation requires DcapRequired mode.
    #[error("production quote generation requires DcapRequired attestation mode")]
    QuoteRequiresDcapRequired,
    /// Remote session authorization requires initialized production state.
    #[error("remote session authorization requires initialized production state")]
    RemoteSessionRequiresProdInit,
    /// The remote session authorization mutex is poisoned.
    #[error("remote session authorization lock is poisoned")]
    RemoteSessionAuthLockPoisoned,
    /// The remote session authorization generation predates retirement.
    #[error("remote session authorization predates enclave retirement")]
    RemoteSessionPredatesRetirement,
    /// The pending remote session table is full.
    #[error("pending remote session capacity reached")]
    RemoteSessionCapacityReached,
    /// The remote session ticket ID already exists.
    #[error("remote session ticket already exists")]
    RemoteSessionTicketExists,
    /// The remote retirement height is zero.
    #[error("remote retirement height is zero")]
    RemoteRetirementHeightZero,
    /// The remote admission mutex is poisoned.
    #[error("remote admission lock poisoned")]
    RemoteAdmissionLockPoisoned,
    /// The remote session was retired at an enclave upgrade deadline.
    #[error("remote session retired at enclave upgrade deadline")]
    RemoteSessionRetiredAtUpgradeDeadline,
    /// The remote session ticket ID is invalid.
    #[error("remote session ticket is invalid")]
    RemoteSessionTicketInvalid,
    /// The remote session ticket is missing, expired, or already consumed.
    #[error("remote session ticket is unavailable, expired, or already used")]
    RemoteSessionTicketUnavailable,
    /// The remote session authorization fields are malformed.
    #[error("remote session authorization is malformed")]
    RemoteSessionAuthMalformed,
    /// The remote session targets a different Noise responder key.
    #[error("remote session targets another Noise responder")]
    RemoteSessionWrongResponder,
    /// The remote session authorization deadline has passed.
    #[error("remote session authorization is expired")]
    RemoteSessionAuthExpired,
    /// The manifest challenge does not match the enclave challenge.
    #[error("initialization challenge mismatch")]
    ChallengeMismatch,
    /// The manifest public keys do not match the enclave persistent keys.
    #[error("initialization manifest does not bind this enclave's persistent keys")]
    ManifestDoesNotBindPersistentKeys,
    /// The manifest network binding differs from the sealed enclave identity.
    #[error("initialization network binding differs from the sealed enclave identity")]
    NetworkBindingDiffersFromSealedIdentity,
    /// The manifest chain ID does not match the boot config chain ID.
    #[error("initialization chain id does not match enclave boot config")]
    ChainIdMismatchBootConfig,
    /// The manifest network binding differs from the release descriptor.
    #[error("initialization network binding differs from the release-measured descriptor")]
    NetworkBindingDiffersFromDescriptor,
    /// The manifest chain ID slice conversion failed.
    #[error("initialization chain id has invalid width")]
    InvalidChainIdWidth,
    /// The attestation mode is forbidden for the target chain ID.
    #[error("attestation mode is forbidden for the initialized network")]
    AttestationModeForbidden,
    /// The runtime attestation mechanism does not match the manifest mode.
    #[error("runtime attestation {runtime} does not satisfy {mode:?}")]
    RuntimeAttestationDoesNotSatisfy {
        runtime: String,
        mode: AttestationMode,
    },
    /// Reading the release-measured network descriptor failed.
    #[error("SGX initialization requires release-measured network descriptor {path}: {detail}")]
    MissingReleaseDescriptor { path: &'static str, detail: String },
    /// Decoding the release-measured network descriptor failed.
    #[error("release-measured network descriptor {path} is invalid: {detail}")]
    InvalidReleaseDescriptor { path: &'static str, detail: String },
    /// Writing the sealed node authorization file failed.
    #[error("persist sealed node authorization: {0}")]
    PersistSealedNodeAuth(String),
    /// Reading the sealed identity file failed.
    #[error("read sealed identity: {0}")]
    ReadSealedIdentity(String),
    /// Unsealing the existing sealed identity failed.
    #[error("verify sealed identity: {0}")]
    VerifySealedIdentity(String),
    /// The existing sealed identity does not match the initialization state.
    #[error("sealed enclave identity differs from initialization")]
    SealedIdentityDiffersFromInit,
    /// Sealing the enclave identity failed.
    #[error("seal enclave identity: {0}")]
    SealEnclaveIdentity(String),
    /// Writing the sealed enclave identity file failed.
    #[error("persist sealed enclave identity: {0}")]
    PersistSealedEnclaveIdentity(String),
    /// Reading the sealed node authorization file failed.
    #[error("read sealed node authorization: {0}")]
    ReadSealedNodeAuth(String),
    /// Unsealing the node authorization file failed.
    #[error("unseal node authorization: {0}")]
    UnsealNodeAuth(String),
    /// The unsealed node authorization failed canonical decoding.
    #[error("sealed node authorization is non-canonical: {0}")]
    SealedNodeAuthNonCanonical(String),
    /// The unsealed node authorization hash does not match the manifest hash.
    #[error("sealed node authorization hash mismatch")]
    SealedNodeAuthHashMismatch,
    /// The unsealed node authorization does not match the enclave identity.
    #[error("sealed node authorization does not match this enclave identity")]
    SealedNodeAuthDoesNotMatchIdentity,
    /// Production initialization requires an SGX sealing key.
    #[error("production initialization requires an SGX sealing key")]
    MissingSgxSealingKey,
    /// A production DCAP release binary refused a non-DCAP runtime mode.
    #[error("production DCAP release refuses runtime attestation {0}")]
    ProdDcapReleaseRefusesAttestation(String),
    /// The measured consensus chain ID exceeds u64.
    #[error("measured consensus chain id does not fit u64")]
    MeasuredChainIdOverflow,
    /// Binding the measured consensus chain ID failed.
    #[error("bind measured consensus chain id: {0}")]
    BindConsensusChainId(String),
    /// Local E2E software sealing is not configured.
    #[error("E2E sealing is not configured")]
    E2eSealingNotConfigured,
    /// The initialization challenge is all zeros.
    #[error("initialization challenge must be nonzero")]
    ZeroChallenge,
    /// An attestation primitive call returned an error.
    #[error("{0}")]
    AttestationPrimitive(String),
}

impl InitError {
    /// Return a static string for session liveness errors.
    pub const fn as_static_str(&self) -> &'static str {
        match self {
            Self::SystemTimePrecedesEpoch => "system time precedes Unix epoch",
            Self::RemoteSessionLeaseExpired => "remote session lease expired",
            _ => "initialization state unavailable",
        }
    }

    /// Wrap a non-canonical manifest decode error.
    pub fn manifest_not_canonical(err: impl fmt::Display) -> Self {
        Self::ManifestNotCanonical(err.to_string())
    }

    /// Wrap a non-canonical registration intent decode error.
    pub fn registration_intent_not_canonical(err: impl fmt::Display) -> Self {
        Self::IntentNotCanonical(err.to_string())
    }

    /// Wrap an attestation primitive error.
    pub fn other(err: impl fmt::Display) -> crate::errors::EnclaveError {
        Self::AttestationPrimitive(err.to_string()).into()
    }

    /// Wrap a missing release network descriptor error.
    pub fn read_network_descriptor(path: &'static str, err: impl fmt::Display) -> Self {
        Self::MissingReleaseDescriptor {
            path,
            detail: err.to_string(),
        }
    }

    /// Wrap an invalid release network descriptor error.
    pub fn invalid_network_descriptor(path: &'static str, err: impl fmt::Display) -> Self {
        Self::InvalidReleaseDescriptor {
            path,
            detail: err.to_string(),
        }
    }

    /// Wrap a sealed node authorization write error.
    pub fn persist_sealed_node_auth(err: impl fmt::Display) -> crate::errors::EnclaveError {
        Self::PersistSealedNodeAuth(err.to_string()).into()
    }

    /// Wrap a sealed identity read error.
    pub fn read_sealed_identity(err: impl fmt::Display) -> Self {
        Self::ReadSealedIdentity(err.to_string())
    }

    /// Wrap a sealed identity verification error.
    pub fn verify_sealed_identity(err: impl fmt::Display) -> Self {
        Self::VerifySealedIdentity(err.to_string())
    }

    /// Wrap an enclave identity seal error.
    pub fn seal_enclave_identity(err: impl fmt::Display) -> Self {
        Self::SealEnclaveIdentity(err.to_string())
    }

    /// Wrap a sealed enclave identity write error.
    pub fn persist_sealed_identity(err: impl fmt::Display) -> crate::errors::EnclaveError {
        Self::PersistSealedEnclaveIdentity(err.to_string()).into()
    }

    /// Wrap a sealed node authorization read error.
    pub fn read_sealed_node_auth(err: impl fmt::Display) -> crate::errors::EnclaveError {
        Self::ReadSealedNodeAuth(err.to_string()).into()
    }

    /// Wrap a node authorization unseal error.
    pub fn unseal_node_auth(err: impl fmt::Display) -> Self {
        Self::UnsealNodeAuth(err.to_string())
    }

    /// Wrap a non-canonical sealed node authorization error.
    pub fn sealed_node_auth_non_canonical(err: impl fmt::Display) -> Self {
        Self::SealedNodeAuthNonCanonical(err.to_string())
    }

    /// Wrap a consensus chain ID bind error.
    pub fn bind_measured_chain_id(err: impl fmt::Display) -> Self {
        Self::BindConsensusChainId(err.to_string())
    }
}

/// Error from streaming onboarding artifact upload and admission verification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OnboardingUploadError {
    /// Upgrade key transfer requires a measured network descriptor.
    #[error("upgrade requires a measured network descriptor")]
    UpgradeRequiresNetworkDescriptor,
    /// The upgrade artifact failed canonical decoding.
    #[error("invalid upgrade artifact")]
    InvalidUpgradeArtifact,
    /// The upgrade transfer request hash does not match the commitment.
    #[error("upgrade transfer commitment mismatch")]
    UpgradeCommitmentMismatch,
    /// The upgrade upload state is missing after initialization.
    #[error("upgrade upload missing")]
    UpgradeUploadMissing,
    /// Onboarding ingest requires a release-measured network descriptor.
    #[error("onboarding ingest requires a release-measured network descriptor")]
    IngestRequiresNetworkDescriptor,
    /// The request is not part of onboarding artifact ingest.
    #[error("request is not part of onboarding artifact ingest")]
    NotOnboardingIngestRequest,
    /// An onboarding artifact upload is already active.
    #[error("an onboarding artifact upload is already active")]
    UploadAlreadyActive,
    /// The onboarding artifact length is zero or exceeds the cap.
    #[error("onboarding artifact length is invalid")]
    ArtifactLenInvalid,
    /// The onboarding artifact failed canonical decoding.
    #[error("onboarding artifact is not canonical")]
    ArtifactNotCanonical,
    /// The onboarding artifact context does not match expected Registry values.
    #[error("onboarding artifact does not match the expected Registry values")]
    ArtifactRegistryValuesMismatch,
    /// The onboarding artifact request hash does not match the commitment.
    #[error("onboarding artifact request commitment mismatch")]
    CommitmentMismatch,
    /// No onboarding artifact upload is active.
    #[error("no onboarding artifact upload is active")]
    NoUploadActive,
    /// A chunk arrived out of sequence.
    #[error("onboarding artifact chunk is out of sequence")]
    ChunkOutOfSequence,
    /// A chunk is empty or exceeds the chunk size cap.
    #[error("onboarding artifact chunk length is invalid")]
    ChunkLenInvalid,
    /// Record kinds were interleaved without committing the active record.
    #[error("onboarding artifact record kinds cannot be interleaved")]
    RecordKindsInterleaved,
    /// The chunk offset does not fit in usize.
    #[error("onboarding artifact chunk offset exceeds this target")]
    ChunkOffsetTarget,
    /// The chunk offset does not match the accumulated record length.
    #[error("onboarding artifact chunks are not strictly sequential")]
    ChunksNotSequential,
    /// Adding the chunk length to the offset overflowed.
    #[error("onboarding artifact chunk offset overflow")]
    ChunkOffsetOverflow,
    /// The accumulated record exceeds its byte cap.
    #[error("onboarding artifact record exceeds its byte cap")]
    RecordExceedsByteCap,
    /// The next chunk offset exceeds u32.
    #[error("onboarding artifact chunk offset exceeds u32")]
    ChunkOffsetU32,
    /// A record commit arrived out of sequence.
    #[error("onboarding artifact record commit is out of sequence")]
    RecordCommitOutOfSequence,
    /// A finish request arrived out of sequence.
    #[error("onboarding artifact finish is out of sequence")]
    FinishOutOfSequence,
    /// Finish was called before an admission record was verified.
    #[error("onboarding artifact has no verified admission record")]
    NoVerifiedAdmissionRecord,
    /// Request commitment hashing failed.
    #[error("{0}")]
    CommitmentHash(String),
}

impl OnboardingUploadError {
    /// Wrap a commitment hash error.
    pub fn other(err: impl fmt::Display) -> Self {
        Self::CommitmentHash(err.to_string())
    }
}

/// Error from combined SGX sealing and hardware sealing probes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SgxSealingError {
    /// The SGX KEYREQUEST buffer length is not 512 bytes.
    #[error("invalid SGX key request length")]
    InvalidKeyRequestLen,
    /// The CPU SVN slice could not be converted.
    #[error("invalid CPU SVN")]
    InvalidCpuSvn,
    /// The SGX KEYREQUEST bytes are not canonical.
    #[error("non-canonical combined SGX sealing request")]
    NonCanonicalCombinedRequest,
    /// Reading `/dev/attestation/report` for sealing failed.
    #[error("read SGX sealing report: {0}")]
    ReadSealingReport(String),
    /// The local SGX report length is not 432 bytes.
    #[error("invalid local SGX report length")]
    InvalidLocalReportLen,
    /// The sealing SVN differs from the local SGX report SVN.
    #[error("sealing SVN differs from local SGX report")]
    SealingSvnMismatch,
    /// The local CPU SVN slice could not be converted.
    #[error("invalid local CPU SVN")]
    InvalidLocalCpuSvn,
    /// SGX sealing is unavailable on this runtime.
    #[error("SGX sealing is unavailable: {0}")]
    SgxSealingUnavailable(String),
    /// The combined SGX EGETKEY instruction returned a non-zero status.
    #[error("combined SGX EGETKEY failed: {0:#x}")]
    CombinedEgetkeyFailed(u64),
    /// Combined SGX sealing requires an x86_64 target.
    #[error("combined SGX sealing requires x86_64")]
    RequiresX86_64,
    /// The TSGX1 envelope is shorter than the header plus KEYREQUEST.
    #[error("truncated combined SGX seal")]
    TruncatedCombinedSeal,
    /// The inner TSEAL header does not match the outer KEYREQUEST metadata.
    #[error("combined seal metadata mismatch")]
    CombinedSealMetadataMismatch,
    /// No SGX sealing key is available.
    #[error("SGX sealing key unavailable")]
    SealingKeyUnavailable,
    /// The legacy TSEAL key policy does not match the runtime key policy.
    #[error("legacy seal policy mismatch")]
    LegacySealPolicyMismatch,
    /// The unsealed network binding does not match the expected binding.
    #[error("sealed blob network mismatch")]
    SealedBlobNetworkMismatch,
    /// The hardware report length in the sealing probe is invalid.
    #[error("invalid hardware report")]
    InvalidHardwareReport,
    /// The sealing probe did not produce a TSGX1 hardware seal.
    #[error("probe did not use hardware sealing")]
    ProbeDidNotUseHardwareSealing,
    /// The unsealed probe payload differs from the test input.
    #[error("probe payload differs")]
    ProbePayloadDiffers,
    /// The sealing probe action argument was neither seal nor unseal.
    #[error("expected seal or unseal")]
    ExpectedSealOrUnseal,
    /// The DCAP quote report data in the probe differs from the test input.
    #[error("quote report data differs")]
    QuoteReportDataDiffers,
    /// Local E2E software sealing only accepts DirectDev devnet bindings.
    #[error("local E2E fixture accepts only DirectDev devnet")]
    LocalE2eDirectDevOnly,
    /// The blob does not start with a simulated local seal prefix.
    #[error("not an explicitly simulated local seal")]
    NotSimulatedLocalSeal,
    /// The simulated local seal header policy is not Mock.
    #[error("invalid local seal policy")]
    InvalidLocalSealPolicy,
    /// An underlying seal, unseal, or I/O operation failed.
    #[error("{0}")]
    Operation(String),
}

impl SgxSealingError {
    /// Wrap a sealing report read error.
    pub fn read_sealing_report(err: impl fmt::Display) -> Self {
        Self::ReadSealingReport(err.to_string())
    }

    /// Wrap an SGX sealing capability probe error.
    pub fn sgx_sealing_unavailable(err: impl fmt::Display) -> Self {
        Self::SgxSealingUnavailable(err.to_string())
    }

    /// Wrap an underlying operation error.
    pub fn operation(err: impl fmt::Display) -> Self {
        Self::Operation(err.to_string())
    }

    /// Alias for [`Self::operation`].
    pub fn other(err: impl fmt::Display) -> Self {
        Self::Operation(err.to_string())
    }

    /// Construct a missing CLI argument error.
    pub fn missing_arg(name: impl fmt::Display) -> Self {
        Self::Operation(format!("missing {name}"))
    }

    /// Construct an invalid CLI argument error.
    pub fn invalid_arg(name: impl fmt::Display) -> Self {
        Self::Operation(format!("invalid {name}"))
    }

    /// Construct an endpoint parse error.
    pub fn endpoint(err: impl fmt::Display) -> Self {
        Self::Operation(format!("invalid endpoint: {err}"))
    }
}

/// Error from enclave binary CLI argument resolution and identity startup.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// `--dev-network-binding` was passed to the production binary.
    #[error("--dev-network-binding is accepted only by the mock binary")]
    DevNetworkBindingMockOnly,
    /// The mock binary was started without `--dev-network-binding`.
    #[error("mock binary requires --dev-network-binding <canonical NetworkBindingV1 hex>")]
    MockRequiresDevNetworkBinding,
    /// Decoding the `--dev-network-binding` hex string failed.
    #[error("decode --dev-network-binding hex: {0}")]
    DecodeDevNetworkBindingHex(String),
    /// Decoding the canonical `NetworkBindingV1` bytes failed.
    #[error("decode canonical NetworkBindingV1: {0}")]
    DecodeCanonicalNetworkBinding(String),
    /// The development network binding does not use GramineDirectDev mode.
    #[error("development network binding must use GramineDirectDev mode")]
    DevNetworkBindingRequiresDirectDev,
    /// The development network binding chain ID disagrees with `--chain-id`.
    #[error("development network binding chain id does not match --chain-id")]
    DevNetworkBindingChainIdMismatch,
    /// A development DKG seed was passed to the production binary.
    #[error("--dkg-seed and TEE_DEV_DKG_SEED are development-only")]
    DkgSeedDevOnly,
    /// Production startup omitted `--tee-dir`.
    #[error("production requires --tee-dir for sealed identity")]
    ProdRequiresTeeDir,
    /// Production identity initialization found no SGX sealing key.
    #[error("production identity requires an SGX sealing key")]
    ProdIdentityRequiresSgxSealingKey,
    /// Creating the identity directory failed.
    #[error("create identity directory {path}: {detail}")]
    CreateIdentityDirectory { path: String, detail: String },
    /// Setting permissions on the identity directory failed.
    #[error("chmod identity directory {path}: {detail}")]
    ChmodIdentityDirectory { path: String, detail: String },
    /// Reading the sealed identity file failed.
    #[error("read sealed identity {path}: {detail}")]
    ReadSealedIdentity { path: String, detail: String },
    /// Unsealing the sealed identity file failed.
    #[error("sealed identity at {path} is unusable and will not be replaced: {detail}")]
    UnusableSealedIdentity { path: String, detail: String },
    /// The sealed identity file contained unexpected group signature bytes.
    #[error("sealed identity at {path} contains unexpected group-signature bytes")]
    SealedIdentityUnexpectedGroupSig { path: String },
    /// Node authorization exists on disk while the sealed identity file is missing.
    #[error(
        "sealed identity is missing while node authorization exists at {path}; old identity cannot be recovered"
    )]
    OrphanedNodeAuthorization { path: String },
    /// Inspecting the sealed node authorization file failed.
    #[error("inspect sealed node authorization {path}: {detail}")]
    InspectSealedNodeAuthorization { path: String, detail: String },
}

impl RunError {
    /// Wrap a `--dev-network-binding` hex decode error.
    pub fn decode_dev_network_binding_hex(err: impl fmt::Display) -> Self {
        Self::DecodeDevNetworkBindingHex(err.to_string())
    }

    /// Wrap a `NetworkBindingV1` canonical decode error.
    pub fn decode_canonical_network_binding(err: impl fmt::Display) -> Self {
        Self::DecodeCanonicalNetworkBinding(err.to_string())
    }

    /// Wrap an identity directory creation error.
    pub fn create_identity_dir(path: impl fmt::Display, err: impl fmt::Display) -> Self {
        Self::CreateIdentityDirectory {
            path: path.to_string(),
            detail: err.to_string(),
        }
    }

    /// Wrap an identity directory chmod error.
    pub fn chmod_identity_dir(path: impl fmt::Display, err: impl fmt::Display) -> Self {
        Self::ChmodIdentityDirectory {
            path: path.to_string(),
            detail: err.to_string(),
        }
    }

    /// Wrap a sealed identity read error.
    pub fn read_sealed_identity(
        path: impl fmt::Display,
        err: impl fmt::Display,
    ) -> crate::errors::EnclaveError {
        Self::ReadSealedIdentity {
            path: path.to_string(),
            detail: err.to_string(),
        }
        .into()
    }

    /// Wrap an unusable sealed identity error.
    pub fn unusable_sealed_identity(path: impl fmt::Display, err: impl fmt::Display) -> Self {
        Self::UnusableSealedIdentity {
            path: path.to_string(),
            detail: err.to_string(),
        }
    }

    /// Wrap a sealed node authorization metadata inspection error.
    pub fn inspect_sealed_node_auth(
        path: impl fmt::Display,
        err: impl fmt::Display,
    ) -> crate::errors::EnclaveError {
        Self::InspectSealedNodeAuthorization {
            path: path.to_string(),
            detail: err.to_string(),
        }
        .into()
    }
}
