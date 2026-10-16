//! Signed inventory schema. Native progress is observed, never normalized.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use thiserror::Error;

pub const MANIFEST_VERSION: u32 = 1;

/// All hashes in the manifest use 64 lowercase hex digits, without a prefix.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockIdentity {
    pub number: u64,
    pub hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnwindProgress {
    pub finish_block_number: u64,
    pub partial_state_trie: u64,
}

/// Independent native observations. A tail above finalized is preserved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProgress {
    pub finalized: BlockIdentity,
    pub execution: BlockIdentity,
    pub execution_stage: Option<u64>,
    pub finish_stage: Option<u64>,
    pub partial_state_trie: Option<u64>,
    pub unwind: Option<UnwindProgress>,
    pub storage_version: u32,
    pub ocomp_baseline: BlockIdentity,
    pub ocomp_previous: BlockIdentity,
    pub ocomp_current: BlockIdentity,
}

/// Finite public data classes. Secret/configuration domains have no variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DomainKind {
    ExecutionDb,
    StaticFiles,
    ExecutionRocksDb,
    MarshalFinalizations,
    MarshalBlocks,
    MarshalMetadata,
    MarshalCache,
    ParentCertificates,
    OcompRetention,
    ClosureCheckpoint,
    Discovery,
    ProtocolBundles,
    CasObjects,
    InputReferences,
    ExportReceipts,
    ExportBindings,
    JobPublicRecords,
    MaterializationReferences,
    LocalResults,
    ExexCheckpoint,
    FatalEvidence,
}

/// Informational ordinary configuration root. It is never an extraction destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeRoot {
    Chain,
    Consensus,
    Ocomp,
    StaticFiles,
    ExecutionRocksDb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub sha256: Option<String>,
    /// Native permission bits. Ordinary file placement handles ownership.
    pub mode: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainInventory {
    /// Unique portable label, also the domain's archive member prefix.
    pub id: String,
    pub kind: DomainKind,
    pub native_root: NativeRoot,
    /// The recorded native location. File placement remains an operator action.
    pub native_path: String,
    pub mode: u32,
    /// Entries in their declared order, including explicit empty directories.
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifestV1 {
    pub version: u32,
    pub chain_id: u64,
    pub genesis_hash: String,
    pub created_at_unix: u64,
    pub creator: Option<String>,
    pub source: Option<String>,
    pub progress: NativeProgress,
    pub domains: Vec<DomainInventory>,
    pub file_count: u64,
    pub total_bytes: u64,
}

impl SnapshotManifestV1 {
    pub fn from_bytes(raw: &[u8]) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_slice(raw).map_err(ManifestError::Json)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Structural inventory validation, not a claim about native data validity.
    pub fn validate(&self) -> Result<(), ManifestError> {
        require(
            self.version == MANIFEST_VERSION,
            ManifestError::UnsupportedManifestVersion,
        )?;
        require(
            matches!(self.progress.storage_version, 1 | 2),
            ManifestError::UnsupportedNativeStorageVersion,
        )?;
        require(
            is_hash(&self.genesis_hash),
            ManifestError::InvalidGenesisHash,
        )?;
        for block in [
            &self.progress.finalized,
            &self.progress.execution,
            &self.progress.ocomp_baseline,
            &self.progress.ocomp_previous,
            &self.progress.ocomp_current,
        ] {
            require(
                is_hash(&block.hash),
                ManifestError::InvalidNativeCheckpointHash,
            )?;
        }
        let mut domain_ids = BTreeSet::new();
        let mut count = 0_u64;
        let mut bytes = 0_u64;
        for domain in &self.domains {
            require(!domain.id.is_empty(), ManifestError::EmptyDomainIdentifier)?;
            require(
                domain_ids.insert(domain.id.as_str()),
                ManifestError::DuplicateDomainIdentifier,
            )?;
            let mut member_paths = BTreeSet::new();
            for entry in &domain.entries {
                require(
                    member_paths.insert(entry.path.as_str()),
                    ManifestError::DuplicateMemberEntry,
                )?;
                match entry.kind {
                    EntryKind::File => {
                        require(
                            entry.sha256.as_deref().is_some_and(is_hash),
                            ManifestError::InvalidFileDigest,
                        )?;
                        count = count
                            .checked_add(1)
                            .ok_or(ManifestError::FileCountOverflow)?;
                        bytes = bytes
                            .checked_add(entry.size)
                            .ok_or(ManifestError::ByteCountOverflow)?;
                    }
                    EntryKind::Directory => require(
                        entry.size == 0 && entry.sha256.is_none(),
                        ManifestError::DirectoryDeclaresFileContents,
                    )?,
                }
            }
        }
        require(
            count == self.file_count && bytes == self.total_bytes,
            ManifestError::InventoryTotalsDisagree,
        )
    }
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid manifest JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid manifest: unsupported manifest version")]
    UnsupportedManifestVersion,
    #[error("invalid manifest: unsupported native storage version")]
    UnsupportedNativeStorageVersion,
    #[error("invalid manifest: invalid genesis hash")]
    InvalidGenesisHash,
    #[error("invalid manifest: invalid native checkpoint hash")]
    InvalidNativeCheckpointHash,
    #[error("invalid manifest: empty domain identifier")]
    EmptyDomainIdentifier,
    #[error("invalid manifest: duplicate domain identifier")]
    DuplicateDomainIdentifier,
    #[error("invalid manifest: duplicate member entry")]
    DuplicateMemberEntry,
    #[error("invalid manifest: invalid file digest")]
    InvalidFileDigest,
    #[error("invalid manifest: file count overflow")]
    FileCountOverflow,
    #[error("invalid manifest: byte count overflow")]
    ByteCountOverflow,
    #[error("invalid manifest: directory must not declare file contents")]
    DirectoryDeclaresFileContents,
    #[error("invalid manifest: inventory totals disagree")]
    InventoryTotalsDisagree,
}

fn require(condition: bool, error: ManifestError) -> Result<(), ManifestError> {
    if condition {
        Ok(())
    } else {
        Err(error)
    }
}

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Hash the exact received/produced bytes, not a parsed-and-reserialized value.
pub fn manifest_digest(raw: &[u8]) -> [u8; 32] {
    Sha256::digest(raw).into()
}
