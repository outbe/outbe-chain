//! Snapshot manifest and I/O error types.

use std::{io, path::PathBuf};
use thiserror::Error;

pub use crate::manifest::ManifestError;

#[derive(Debug, Error)]
pub enum ArchiveReadError {
    #[error("undeclared archive payload")]
    UndeclaredArchivePayload,
    #[error("missing {name}")]
    MissingMetadata { name: String },
    #[error("invalid {name} archive member")]
    InvalidArchiveMember { name: String },
    #[error("missing payload domain")]
    MissingPayloadDomain,
    #[error("payload domain differs from manifest")]
    PayloadDomainMismatch,
    #[error("payload root mode differs from signed entry")]
    PayloadRootModeMismatch,
    #[error("missing declared payload")]
    MissingDeclaredPayload,
    #[error("payload member differs from manifest")]
    PayloadMemberMismatch,
    #[error("payload checksum mismatch: {path}")]
    PayloadChecksumMismatch { path: String },
    #[error("nonzero data after archive terminator")]
    NonzeroTailData,
}

impl From<ArchiveReadError> for io::Error {
    fn from(error: ArchiveReadError) -> Self {
        Self::new(io::ErrorKind::InvalidData, error)
    }
}

#[derive(Debug, Error)]
pub enum SnapshotCreateError {
    #[error("native domain/root count differs")]
    DomainRootCountMismatch,
    #[error("file count overflow")]
    FileCountOverflow,
    #[error("snapshot byte count overflow")]
    ByteCountOverflow,
    #[error("snapshot source checksum changed: {path}")]
    SourceChecksumChanged { path: String },
}

impl From<SnapshotCreateError> for io::Error {
    fn from(error: SnapshotCreateError) -> Self {
        Self::other(error)
    }
}

#[derive(Debug, Error)]
pub enum SnapshotFsError {
    #[error("snapshot source root changed: {}", path.display())]
    SourceRootChanged { path: PathBuf },
    #[error("snapshot source changed: {}", path.display())]
    SourceChanged { path: PathBuf },
    #[error("snapshot source changed while reading")]
    SourceChangedWhileReading,
    #[error("pending snapshot file was replaced")]
    PendingFileReplaced,
}

impl From<SnapshotFsError> for io::Error {
    fn from(error: SnapshotFsError) -> Self {
        Self::other(error)
    }
}

#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("native domain labels must be nonempty and unique")]
    InvalidDomainLabels,
    #[error("snapshot output or scratch overlaps a protected path")]
    OutputOverlapsProtectedPath,
    #[error("snapshot output and scratch paths overlap")]
    OutputAndScratchOverlap,
    #[error("native source domains overlap")]
    SourceDomainsOverlap,
    #[error("native source overlaps protected identity or configuration")]
    SourceOverlapsProtectedPath,
    #[error("snapshot output or scratch overlaps a native source")]
    OutputOverlapsNativeSource,
    #[error("filesystem path must not be empty")]
    EmptyFilesystemPath,
    #[error("unresolvable filesystem path")]
    UnresolvableFilesystemPath,
    #[error("filesystem path has no existing ancestor")]
    NoExistingAncestor,
}

impl From<LayoutError> for io::Error {
    fn from(error: LayoutError) -> Self {
        Self::new(io::ErrorKind::InvalidInput, error)
    }
}

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("unsupported snapshot signature format")]
    UnsupportedSignatureFormat,
    #[error("signed manifest digest differs")]
    SignedManifestDigestMismatch,
    #[error("snapshot signature does not match declared creator")]
    CreatorSignatureMismatch,
    #[error("snapshot creator does not match expected public key")]
    ExpectedPublicKeyMismatch,
    #[error("snapshot signature must use low S")]
    NonLowSSignature,
    #[error("invalid snapshot signature recovery ID")]
    InvalidRecoveryId,
}

impl From<ProvenanceError> for io::Error {
    fn from(error: ProvenanceError) -> Self {
        Self::new(io::ErrorKind::InvalidData, error)
    }
}
