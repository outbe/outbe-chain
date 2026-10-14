//! Typed error definitions for DCAP verification, finalized admission, and Gramine.

use std::fmt;

/// Error from in-enclave DCAP quote verification and onboarding artifact creation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DcapVerifyError {
    /// The onboarding node signature length is not 65 bytes.
    #[error("DCAP onboarding node signature length is invalid")]
    OnboardingNodeSignatureLen,
    /// The onboarding enclave signature length is not 64 bytes.
    #[error("DCAP onboarding enclave signature length is invalid")]
    OnboardingEnclaveSignatureLen,
    /// The request is not part of the DCAP verification upload protocol.
    #[error("request is not part of DCAP verification upload")]
    NotUploadRequest,
    /// A DCAP verification upload is already active on this session.
    #[error("a DCAP verification upload is already active")]
    UploadAlreadyActive,
    /// The evidence length does not fit in usize.
    #[error("DCAP evidence length exceeds this target")]
    EvidenceLenTarget,
    /// The policy length does not fit in usize.
    #[error("DCAP policy length exceeds this target")]
    PolicyLenTarget,
    /// The evidence length exceeds the protocol maximum.
    #[error("DCAP evidence length exceeds the protocol cap")]
    EvidenceLenCap,
    /// The policy length exceeds the protocol maximum.
    #[error("DCAP policy length exceeds the protocol cap")]
    PolicyLenCap,
    /// Adding evidence and policy lengths overflowed.
    #[error("DCAP verification upload length overflow")]
    UploadLenOverflow,
    /// No DCAP verification upload is active on this session.
    #[error("no DCAP verification upload is active")]
    NoUploadActive,
    /// The chunk request hash does not match the active upload.
    #[error("DCAP verification chunk request hash mismatch")]
    ChunkHashMismatch,
    /// The chunk length is zero or exceeds the chunk byte cap.
    #[error("DCAP verification chunk length is invalid")]
    ChunkLenInvalid,
    /// Converting or advancing the chunk offset overflowed.
    #[error("DCAP verification offset overflow")]
    OffsetOverflow,
    /// The chunk offset does not match the current buffer length.
    #[error("DCAP verification chunk offset is not sequential")]
    ChunkOffsetNonSequential,
    /// Adding the chunk length to the current offset overflowed.
    #[error("DCAP verification chunk length overflow")]
    ChunkLenOverflow,
    /// The chunk extends beyond the declared total upload length.
    #[error("DCAP verification chunk exceeds the declared upload length")]
    ChunkExceedsDeclaredLen,
    /// The finish request hash does not match the active upload.
    #[error("DCAP verification finish request hash mismatch")]
    FinishHashMismatch,
    /// The uploaded bytes are shorter than the declared total length.
    #[error("DCAP verification upload is truncated")]
    UploadTruncated,
    /// Computing the DCAP verification request commitment failed.
    #[error("DCAP verification request commitment is invalid")]
    CommitmentInvalid,
    /// The computed request commitment does not match `request_hash`.
    #[error("DCAP verification request commitment mismatch")]
    CommitmentMismatch,
    /// The enclave was built without the `native-dcap` feature.
    #[error("enclave was built without the pinned native DCAP verifier")]
    MissingNativeDcapVerifier,
    /// Encoding the DCAP verification outcome failed.
    #[error("enclave produced a non-canonical DCAP outcome")]
    NonCanonicalOutcome,
    /// Encoding the DCAP onboarding artifact failed.
    #[error("enclave produced a non-canonical onboarding artifact")]
    NonCanonicalOnboardingArtifact,
    /// Building the onboarding attestation preimage failed.
    #[error("enclave produced an oversized onboarding result")]
    OversizedOnboardingResult,
    /// Building the verification attestation preimage failed.
    #[error("enclave produced an oversized DCAP outcome")]
    OversizedOutcome,
    /// The onboarding source enclave has no initialization manifest.
    #[error("onboarding source is not initialized")]
    OnboardingSourceNotInitialized,
    /// The DirectDev onboarding context does not match the source network binding.
    #[error("GramineDirectDev onboarding source network binding mismatch")]
    DirectDevSourceNetworkMismatch,
    /// The onboarding source enclave does not have a ready offer key.
    #[error("onboarding source offer key is not ready")]
    OnboardingSourceOfferKeyNotReady,
    /// The DirectDev onboarding context does not match the source offer key.
    #[error("GramineDirectDev onboarding offer-key context mismatch")]
    DirectDevOfferKeyContextMismatch,
    /// Encrypting the DirectDev onboarding artifact failed.
    #[error("purpose-bound GramineDirectDev onboarding artifact encryption failed")]
    DirectDevArtifactEncryptionFailed,
    /// Computing the accepted onboarding intent hash failed.
    #[error("accepted onboarding intent hash is unavailable")]
    OnboardingIntentHashUnavailable,
    /// Computing the accepted onboarding node ID hash failed.
    #[error("accepted onboarding node id hash is unavailable")]
    OnboardingNodeIdHashUnavailable,
    /// Computing the accepted onboarding enclave ID failed.
    #[error("accepted onboarding enclave id is unavailable")]
    OnboardingEnclaveIdUnavailable,
    /// Encrypting the DCAP onboarding artifact failed.
    #[error("purpose-bound onboarding artifact encryption failed")]
    ArtifactEncryptionFailed,
}

/// Error from finalized consensus committee and TEE Registry admission verification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FinalizedAdmissionError {
    /// The onboarding context network does not match the release-measured descriptor.
    #[error("finalized admission network identity is not release-measured")]
    NetworkIdentityNotReleaseMeasured,
    /// Decoding a measured genesis consensus BLS key failed.
    #[error("invalid measured genesis consensus key: {0}")]
    InvalidMeasuredGenesisConsensusKey(String),
    /// A measured genesis consensus key has trailing bytes.
    #[error("measured genesis consensus key has trailing bytes")]
    GenesisConsensusKeyTrailingBytes,
    /// Constructing the ordered genesis committee set failed.
    #[error("invalid measured genesis committee: {0}")]
    InvalidMeasuredGenesisCommittee(String),
    /// The measured consensus chain ID does not fit in u64.
    #[error("measured consensus chain id does not fit u64")]
    ConsensusChainIdOverflow,
    /// Binding the consensus verifier chain ID failed.
    #[error("consensus verifier chain binding rejected: {0}")]
    ConsensusVerifierChainBindingRejected(String),
    /// Registering the epoch-0 finality anchor failed.
    #[error("epoch-0 finality anchor rejected: {0}")]
    Epoch0FinalityAnchorRejected(String),
    /// Decoding the committee transition record failed.
    #[error("committee transition codec: {0}")]
    CommitteeTransitionCodec(String),
    /// Decoding the certified header in a committee transition failed.
    #[error("committee transition decode failed: {0}")]
    CommitteeTransitionDecodeFailed(String),
    /// The committee transition height is not strictly increasing.
    #[error("committee transition envelope or order is invalid")]
    TransitionEnvelopeOrOrder,
    /// No epoch-0 committee was registered in the verifier.
    #[error("epoch-0 committee was not registered")]
    Epoch0NotRegistered,
    /// The transition proof epoch does not match the current registered committee.
    #[error("committee transition was not finalized by the current committee")]
    TransitionNotCurrentCommittee,
    /// Verifying the committee transition finalization certificate failed.
    #[error("committee transition finalization rejected: {0}")]
    TransitionFinalizationRejected(String),
    /// Decoding the block header extra-data artifacts failed.
    #[error("committee transition artifacts rejected: {0}")]
    TransitionArtifactsRejected(String),
    /// The transition block header lacks a `CommitteePreAnnounce` artifact.
    #[error("committee transition lacks a pre-announce")]
    TransitionLacksPreAnnounce,
    /// The announced next epoch is not the immediate successor epoch.
    #[error("committee transition does not announce the immediate successor")]
    TransitionNotImmediateSuccessor,
    /// Registering the successor committee outcome failed.
    #[error("successor committee rejected: {0}")]
    SuccessorCommitteeRejected(String),
    /// Advancing the verified committee finality height overflowed.
    #[error("committee finality height overflow")]
    CommitteeFinalityHeightOverflow,
    /// Decoding the finalized admission witness failed.
    #[error("finalized admission witness codec: {0}")]
    AdmissionWitnessCodec(String),
    /// Decoding the certified header in the admission witness failed.
    #[error("admission finalization decode failed: {0}")]
    AdmissionFinalizationDecodeFailed(String),
    /// The admission block height does not follow the previous verified height.
    #[error("admission finalization envelope or order is invalid")]
    AdmissionEnvelopeOrOrder,
    /// The admission proof epoch does not match the highest registered committee.
    #[error("admission certificate skips an unauthenticated committee")]
    AdmissionSkipsCommittee,
    /// Verifying the admission finalization certificate failed.
    #[error("admission finalization rejected: {0}")]
    AdmissionFinalizationRejected(String),
    /// The Registry storage proof slot count does not match the expected count.
    #[error("finalized admission Registry proof has an unexpected slot count")]
    RegistrySlotCount,
    /// A required TEE Registry storage slot is missing from the proof.
    #[error("finalized admission Registry proof is missing a required slot")]
    RegistryMissingSlot,
    /// A proved TEE Registry slot value differs from the onboarding context.
    #[error("finalized admission Registry value differs from onboarding context")]
    RegistryValueMismatch,
    /// Decoding the public finalization certificate failed.
    #[error("{0}")]
    PublicFinalizationDecode(String),
    /// RLP decoding of the canonical Outbe header failed.
    #[error("invalid canonical Outbe header RLP: {0}")]
    InvalidCanonicalHeaderRlp(String),
    /// Trailing bytes remained after decoding the canonical Outbe header.
    #[error("trailing bytes after canonical Outbe header")]
    HeaderTrailingBytes,
    /// RLP decoding of a descendant header failed.
    #[error("invalid descendant header: {0}")]
    InvalidDescendantHeader(String),
    /// The descendant headers do not form a consecutive parent-hash chain.
    #[error("finality ancestry is not a consecutive parent-hash chain")]
    FinalityAncestryNotConsecutive,
    /// The finalization proposal payload does not match the terminal header hash.
    #[error("finalization payload does not authenticate the header ancestry")]
    FinalizationPayloadMismatch,
    /// The TEE Registry MPT account proof failed verification.
    #[error("finalized admission Registry account proof rejected: {0}")]
    RegistryAccountProofRejected(String),
    /// A TEE Registry MPT storage proof failed verification.
    #[error("finalized admission Registry storage proof rejected: {0}")]
    RegistryStorageProofRejected(String),
    /// A required upgrade authorization slot is missing from the proof.
    #[error("missing upgrade authorization slot")]
    MissingUpgradeSlot,
    /// The upgrade authorization is absent or expired at the proved block.
    #[error("upgrade authorization is absent or expired at the proved block")]
    UpgradeAuthorizationAbsentOrExpired,
    /// The Registry proof is missing the lease expiry slot.
    #[error("finalized admission Registry proof lacks lease expiry")]
    RegistryLacksLeaseExpiry,
    /// The Registry lease expiry is not live at the proved block timestamp.
    #[error("finalized admission Registry lease is not live at the proved block")]
    RegistryLeaseNotLive,
}

impl FinalizedAdmissionError {
    /// Wrap a genesis consensus key decode error.
    pub fn genesis_consensus_key(err: impl fmt::Display) -> Self {
        Self::InvalidMeasuredGenesisConsensusKey(err.to_string())
    }

    /// Wrap a genesis committee construction error.
    pub fn genesis_committee(err: impl fmt::Debug) -> Self {
        Self::InvalidMeasuredGenesisCommittee(format!("{err:?}"))
    }

    /// Wrap a consensus chain binding error.
    pub fn consensus_chain_binding(err: impl fmt::Display) -> Self {
        Self::ConsensusVerifierChainBindingRejected(err.to_string())
    }

    /// Wrap an epoch-0 finality anchor error.
    pub fn epoch0_anchor(err: impl fmt::Display) -> Self {
        Self::Epoch0FinalityAnchorRejected(err.to_string())
    }

    /// Wrap a committee transition codec error.
    pub fn transition_codec(err: impl fmt::Display) -> Self {
        Self::CommitteeTransitionCodec(err.to_string())
    }

    /// Wrap a committee transition header decode error.
    pub fn transition_decode(err: impl fmt::Display) -> Self {
        Self::CommitteeTransitionDecodeFailed(err.to_string())
    }

    /// Wrap a committee transition finalization error.
    pub fn transition_finalization(err: impl fmt::Display) -> Self {
        Self::TransitionFinalizationRejected(err.to_string())
    }

    /// Wrap a committee transition artifacts decode error.
    pub fn transition_artifacts(err: impl fmt::Debug) -> Self {
        Self::TransitionArtifactsRejected(format!("{err:?}"))
    }

    /// Wrap a successor committee registration error.
    pub fn successor_committee(err: impl fmt::Display) -> Self {
        Self::SuccessorCommitteeRejected(err.to_string())
    }

    /// Wrap an admission witness codec error.
    pub fn admission_witness_codec(err: impl fmt::Display) -> Self {
        Self::AdmissionWitnessCodec(err.to_string())
    }

    /// Wrap an admission finalization decode error.
    pub fn admission_decode(err: impl fmt::Display) -> Self {
        Self::AdmissionFinalizationDecodeFailed(err.to_string())
    }

    /// Wrap an admission finalization verification error.
    pub fn admission_finalization(err: impl fmt::Display) -> Self {
        Self::AdmissionFinalizationRejected(err.to_string())
    }

    /// Wrap a public finalization decode error.
    pub fn public_finalization(err: impl fmt::Display) -> Self {
        Self::PublicFinalizationDecode(err.to_string())
    }

    /// Wrap a canonical Outbe header RLP error.
    pub fn canonical_header_rlp(err: impl fmt::Display) -> Self {
        Self::InvalidCanonicalHeaderRlp(err.to_string())
    }

    /// Wrap a descendant header RLP error.
    pub fn descendant_header(err: impl fmt::Display) -> Self {
        Self::InvalidDescendantHeader(err.to_string())
    }

    /// Wrap a Registry account proof verification error.
    pub fn registry_account_proof(err: impl fmt::Display) -> Self {
        Self::RegistryAccountProofRejected(err.to_string())
    }

    /// Wrap a Registry storage proof verification error.
    pub fn registry_storage_proof(err: impl fmt::Display) -> Self {
        Self::RegistryStorageProofRejected(err.to_string())
    }
}

/// Error from Gramine pseudo-filesystem operations and enclave key derivation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GramineError {
    /// Writing a Gramine pseudo-file or capture output file failed.
    #[error("write {path}: {detail}")]
    WritePath { path: String, detail: String },
    /// Reading a Gramine pseudo-file or capture input file failed.
    #[error("read {path}: {detail}")]
    ReadPath { path: String, detail: String },
    /// Creating a capture output file failed.
    #[error("create {path}: {detail}")]
    CreatePath { path: String, detail: String },
    /// Syncing a capture output file failed.
    #[error("sync {path}: {detail}")]
    SyncPath { path: String, detail: String },
    /// The DCAP quote read from Gramine is shorter than the minimum quote length.
    #[error("quote too short: {len} < {min_len}")]
    QuoteTooShort { len: usize, min_len: usize },
    /// Decoding a canonical `RegistrationIntentV1` for fixture capture failed.
    #[error("decode canonical RegistrationIntentV1: {0}")]
    DecodeCanonicalRegistrationIntent(String),
    /// The capture registration intent does not use `DcapRequired` mode.
    #[error("capture intent does not require DCAP")]
    CaptureIntentNotDcap,
    /// Deriving `report_data` from a `RegistrationIntentV1` failed.
    #[error("derive RegistrationIntentV1 report_data: {0}")]
    DeriveRegistrationIntentReportData(String),
    /// The captured quote `REPORT_DATA` does not match the canonical intent.
    #[error("captured quote REPORT_DATA does not match the canonical intent")]
    CapturedReportDataMismatch,
    /// A Gramine sealing key file returned zero bytes.
    #[error("{path} returned empty key")]
    EmptyKey { path: String },
    /// TEE-BLS identity v1 key derivation produced only zero scalars.
    #[error("TEE-BLS identity v1 mapping produced only zero scalars")]
    ZeroTeeBlsScalar,
    /// Enclave key derivation or quote measurement parsing failed.
    #[error("{0}")]
    KeyDerivation(String),
}

impl GramineError {
    /// Wrap a file write error with its path.
    pub fn write_path(path: impl Into<String>, err: impl fmt::Display) -> Self {
        Self::WritePath {
            path: path.into(),
            detail: err.to_string(),
        }
    }

    /// Wrap a file read error with its path.
    pub fn read_path(path: impl Into<String>, err: impl fmt::Display) -> Self {
        Self::ReadPath {
            path: path.into(),
            detail: err.to_string(),
        }
    }

    /// Wrap a file create error with its path.
    pub fn create_path(path: impl Into<String>, err: impl fmt::Display) -> Self {
        Self::CreatePath {
            path: path.into(),
            detail: err.to_string(),
        }
    }

    /// Wrap a file sync error with its path.
    pub fn sync_path(path: impl Into<String>, err: impl fmt::Display) -> Self {
        Self::SyncPath {
            path: path.into(),
            detail: err.to_string(),
        }
    }

    /// Wrap a canonical registration intent decode error.
    pub fn decode_canonical_intent(err: impl fmt::Display) -> Self {
        Self::DecodeCanonicalRegistrationIntent(err.to_string())
    }

    /// Wrap a registration intent `report_data` derivation error.
    pub fn derive_intent_report_data(err: impl fmt::Display) -> Self {
        Self::DeriveRegistrationIntentReportData(err.to_string())
    }

    /// Wrap a quote measurement parse error into `EnclaveError`.
    pub fn quote_measurements(err: impl fmt::Display) -> crate::errors::EnclaveError {
        Self::KeyDerivation(err.to_string()).into()
    }

    /// Wrap a key derivation error.
    pub fn key_derivation(err: impl fmt::Display) -> Self {
        Self::KeyDerivation(err.to_string())
    }
}
