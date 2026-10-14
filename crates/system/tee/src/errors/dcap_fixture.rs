//! Typed errors for the `outbe-dcap-fixture` CLI binary.

use std::fmt;
use std::path::Path;
use thiserror::Error;

/// Errors returned by the `outbe-dcap-fixture` preparation and assembly tool.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum DcapFixtureError {
    /// Caller invoked the CLI with an invalid subcommand.
    #[error(
        "usage: outbe-dcap-fixture prepare --mrenclave <hex32> --mrsigner <hex32> --isv-prod-id <u16> --isv-svn <u16> --timestamp <u64> --output-dir <empty-dir>"
    )]
    Usage,

    /// Required CLI flag was not present.
    #[error("missing {0}")]
    MissingArgument(String),

    /// CLI flag was present without a following value.
    #[error("missing value for {0}")]
    MissingArgumentValue(String),

    /// CLI argument was not a valid `u16`.
    #[error("{0} is not a canonical u16")]
    InvalidU16(String),

    /// CLI argument was not a valid `u64`.
    #[error("{0} is not a canonical u64")]
    InvalidU64(String),

    /// Hex string was not 64 ASCII hexadecimal characters.
    #[error("expected exactly 32 hexadecimal bytes")]
    ExpectedHex32,

    /// Byte was not a valid ASCII hexadecimal digit.
    #[error("invalid hexadecimal digit")]
    InvalidHexDigit,

    /// Output directory already contains files.
    #[error("output directory is not empty: {0}")]
    OutputDirectoryNotEmpty(String),

    /// Filesystem read failed.
    #[error("read {path}: {reason}")]
    ReadPath {
        /// Path that failed to read.
        path: String,
        /// Underlying I/O error message.
        reason: String,
    },

    /// Filesystem create failed.
    #[error("create {path}: {reason}")]
    CreatePath {
        /// Path that failed to create.
        path: String,
        /// Underlying I/O error message.
        reason: String,
    },

    /// Filesystem write failed.
    #[error("write {path}: {reason}")]
    WritePath {
        /// Path that failed to write.
        path: String,
        /// Underlying I/O error message.
        reason: String,
    },

    /// Canonical `TeePolicyV1` encoding failed.
    #[error("encode TeePolicyV1: {0}")]
    EncodeTeePolicy(String),

    /// Canonical `RegistrationIntentV1` encoding failed.
    #[error("encode RegistrationIntentV1: {0}")]
    EncodeRegistrationIntent(String),

    /// `RegistrationIntentV1` report-data derivation failed.
    #[error("derive RegistrationIntentV1 report_data: {0}")]
    DeriveReportData(String),

    /// Capture metadata JSON serialization failed.
    #[error("encode capture metadata: {0}")]
    EncodeCaptureMetadata(String),

    /// Fixture NodeHost signing key construction failed.
    #[error("construct fixture NodeHost key: {0}")]
    ConstructFixtureNodeHostKey(String),

    /// Fixture NodeHost public key encoding failed.
    #[error("encode fixture NodeHost key")]
    EncodeFixtureNodeHostKey,

    /// `TeePolicyV1` hash calculation failed.
    #[error("hash TeePolicyV1: {0}")]
    HashTeePolicy(String),

    /// Lease expiration timestamp addition overflowed `u64`.
    #[error("requested_valid_until overflows u64")]
    RequestedValidUntilOverflowsU64,

    /// Canonical `TeePolicyV1` decoding failed.
    #[error("decode canonical TeePolicyV1: {0}")]
    DecodeTeePolicy(String),

    /// Canonical `RegistrationIntentV1` decoding failed.
    #[error("decode canonical RegistrationIntentV1: {0}")]
    DecodeRegistrationIntent(String),

    /// Captured SGX quote measurement parsing failed.
    #[error("parse captured SGX quote: {0}")]
    ParseCapturedQuote(String),

    /// Captured SGX quote report data differed from the canonical intent.
    #[error("quote REPORT_DATA does not match the canonical intent")]
    ReportDataMismatch,

    /// `capture-provenance.json` failed JSON decoding.
    #[error("decode capture-provenance.json: {0}")]
    DecodeCaptureProvenance(String),

    /// Required collateral component file was missing from the capture set.
    #[error("missing captured component {0}")]
    MissingCapturedComponent(String),

    /// Capture timestamp exceeded the signed 64-bit integer range.
    #[error("timestamp exceeds i64")]
    TimestampExceedsI64,

    /// Native QVL quote verification preflight failed.
    #[error("native QVL preflight failed: {0}")]
    NativeQvlPreflightFailed(String),

    /// Canonical `AttestationEvidenceV1` encoding failed for the valid fixture.
    #[error("encode valid AttestationEvidenceV1: {0}")]
    EncodeValidEvidence(String),

    /// Public DCAP verifier rejected the valid fixture evidence.
    #[error("valid evidence rejected with stable code 0x{code:04x}")]
    ValidEvidenceRejected {
        /// Numeric reject code from the DCAP verifier.
        code: u16,
    },

    /// Canonical `DcapVerdictV1` encoding failed.
    #[error("encode stable verdict 0x{code:04x}")]
    EncodeStableVerdict {
        /// Numeric reject code from the verdict encoder.
        code: u16,
    },

    /// Captured quote was shorter than the ECDSA signature offset.
    #[error("captured quote has no ECDSA signature byte")]
    MissingQuoteSignatureByte,

    /// Canonical `AttestationEvidenceV1` encoding failed for the tampered fixture.
    #[error("encode tampered AttestationEvidenceV1: {0}")]
    EncodeTamperedEvidence(String),

    /// Tampered quote produced an unexpected reject code.
    #[error("tampered quote returned 0x{code:04x}, expected NativeVerificationFailed")]
    UnexpectedTamperedRejectCode {
        /// Numeric reject code returned for the tampered quote.
        code: u16,
    },

    /// Fixture manifest JSON serialization failed.
    #[error("encode fixture manifest: {0}")]
    EncodeFixtureManifest(String),
}

impl DcapFixtureError {
    /// Construct [`Self::MissingArgument`].
    pub fn missing_argument(name: &str) -> Self {
        Self::MissingArgument(name.to_owned())
    }

    /// Construct [`Self::MissingArgumentValue`].
    pub fn missing_argument_value(name: &str) -> Self {
        Self::MissingArgumentValue(name.to_owned())
    }

    /// Construct [`Self::InvalidU16`].
    pub fn invalid_u16<T>(label: &str) -> impl FnOnce(T) -> Self + '_ {
        move |_| Self::InvalidU16(label.to_owned())
    }

    /// Construct [`Self::InvalidU64`].
    pub fn invalid_u64<T>(label: &str) -> impl FnOnce(T) -> Self + '_ {
        move |_| Self::InvalidU64(label.to_owned())
    }

    /// Construct [`Self::ReadPath`].
    pub fn read_path(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |error| Self::ReadPath {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }

    /// Construct [`Self::CreatePath`].
    pub fn create_path(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |error| Self::CreatePath {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }

    /// Construct [`Self::WritePath`].
    pub fn write_path(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |error| Self::WritePath {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }

    /// Construct [`Self::EncodeTeePolicy`].
    pub fn encode_tee_policy(error: impl fmt::Display) -> Self {
        Self::EncodeTeePolicy(error.to_string())
    }

    /// Construct [`Self::EncodeRegistrationIntent`].
    pub fn encode_registration_intent(error: impl fmt::Display) -> Self {
        Self::EncodeRegistrationIntent(error.to_string())
    }

    /// Construct [`Self::DeriveReportData`].
    pub fn derive_report_data(error: impl fmt::Display) -> Self {
        Self::DeriveReportData(error.to_string())
    }

    /// Construct [`Self::EncodeCaptureMetadata`].
    pub fn encode_capture_metadata(error: impl fmt::Display) -> Self {
        Self::EncodeCaptureMetadata(error.to_string())
    }

    /// Construct [`Self::ConstructFixtureNodeHostKey`].
    pub fn construct_fixture_node_host_key(error: impl fmt::Display) -> Self {
        Self::ConstructFixtureNodeHostKey(error.to_string())
    }

    /// Construct [`Self::EncodeFixtureNodeHostKey`].
    pub fn encode_fixture_node_host_key<T>(_: T) -> Self {
        Self::EncodeFixtureNodeHostKey
    }

    /// Construct [`Self::HashTeePolicy`].
    pub fn hash_tee_policy(error: impl fmt::Display) -> Self {
        Self::HashTeePolicy(error.to_string())
    }

    /// Construct [`Self::DecodeTeePolicy`].
    pub fn decode_tee_policy(error: impl fmt::Display) -> Self {
        Self::DecodeTeePolicy(error.to_string())
    }

    /// Construct [`Self::DecodeRegistrationIntent`].
    pub fn decode_registration_intent(error: impl fmt::Display) -> Self {
        Self::DecodeRegistrationIntent(error.to_string())
    }

    /// Construct [`Self::ParseCapturedQuote`].
    pub fn parse_captured_quote(error: impl fmt::Display) -> Self {
        Self::ParseCapturedQuote(error.to_string())
    }

    /// Construct [`Self::DecodeCaptureProvenance`].
    pub fn decode_capture_provenance(error: impl fmt::Display) -> Self {
        Self::DecodeCaptureProvenance(error.to_string())
    }

    /// Construct [`Self::MissingCapturedComponent`].
    pub fn missing_captured_component(name: &str) -> Self {
        Self::MissingCapturedComponent(name.to_owned())
    }

    /// Construct [`Self::TimestampExceedsI64`].
    pub fn timestamp_exceeds_i64<T>(_: T) -> Self {
        Self::TimestampExceedsI64
    }

    /// Construct [`Self::NativeQvlPreflightFailed`].
    pub fn native_qvl_preflight_failed(error: impl fmt::Debug) -> Self {
        Self::NativeQvlPreflightFailed(format!("{error:?}"))
    }

    /// Construct [`Self::EncodeValidEvidence`].
    pub fn encode_valid_evidence(error: impl fmt::Display) -> Self {
        Self::EncodeValidEvidence(error.to_string())
    }

    /// Construct [`Self::ValidEvidenceRejected`].
    pub const fn valid_evidence_rejected(code: crate::dcap_protocol::DcapRejectCodeV1) -> Self {
        Self::ValidEvidenceRejected { code: code.code() }
    }

    /// Construct [`Self::EncodeStableVerdict`].
    pub const fn encode_stable_verdict(code: crate::dcap_protocol::DcapRejectCodeV1) -> Self {
        Self::EncodeStableVerdict { code: code.code() }
    }

    /// Construct [`Self::EncodeTamperedEvidence`].
    pub fn encode_tampered_evidence(error: impl fmt::Display) -> Self {
        Self::EncodeTamperedEvidence(error.to_string())
    }

    /// Construct [`Self::EncodeFixtureManifest`].
    pub fn encode_fixture_manifest(error: impl fmt::Display) -> Self {
        Self::EncodeFixtureManifest(error.to_string())
    }
}
