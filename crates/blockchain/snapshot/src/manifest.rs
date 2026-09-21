//! Signed inventory schema. Native progress is observed, never normalized.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

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

/// Independent native observations; a tail above finalized is preserved.
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
    pub ce: BlockIdentity,
    pub projection: BlockIdentity,
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
    Ce,
    OffchainProjection,
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

/// Informational ordinary configuration root; never an extraction destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeRoot {
    Chain,
    Consensus,
    Ocomp,
    Offchain,
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
    /// Native permission bits; ownership is handled by ordinary file placement.
    pub mode: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainInventory {
    /// Unique portable label, also the domain's archive member prefix.
    pub id: String,
    pub kind: DomainKind,
    pub native_root: NativeRoot,
    /// The recorded native location; file placement remains an operator action.
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
            "unsupported manifest version",
        )?;
        require(
            matches!(self.progress.storage_version, 1 | 2),
            "unsupported native storage version",
        )?;
        require(is_hash(&self.genesis_hash), "invalid genesis hash")?;
        for block in [
            &self.progress.finalized,
            &self.progress.execution,
            &self.progress.ce,
            &self.progress.projection,
            &self.progress.ocomp_baseline,
            &self.progress.ocomp_previous,
            &self.progress.ocomp_current,
        ] {
            require(is_hash(&block.hash), "invalid native checkpoint hash")?;
        }
        let mut domain_ids = BTreeSet::new();
        let mut count = 0_u64;
        let mut bytes = 0_u64;
        for domain in &self.domains {
            require(!domain.id.is_empty(), "empty domain identifier")?;
            require(
                domain_ids.insert(domain.id.as_str()),
                "duplicate domain identifier",
            )?;
            let mut member_paths = BTreeSet::new();
            for entry in &domain.entries {
                require(
                    member_paths.insert(entry.path.as_str()),
                    "duplicate member entry",
                )?;
                match entry.kind {
                    EntryKind::File => {
                        require(
                            entry.sha256.as_deref().is_some_and(is_hash),
                            "invalid file digest",
                        )?;
                        count = count
                            .checked_add(1)
                            .ok_or(ManifestError::Invalid("file count overflow"))?;
                        bytes = bytes
                            .checked_add(entry.size)
                            .ok_or(ManifestError::Invalid("byte count overflow"))?;
                    }
                    EntryKind::Directory => require(
                        entry.size == 0 && entry.sha256.is_none(),
                        "directory must not declare file contents",
                    )?,
                }
            }
        }
        require(
            count == self.file_count && bytes == self.total_bytes,
            "inventory totals disagree",
        )
    }
}

#[derive(Debug)]
pub enum ManifestError {
    Json(serde_json::Error),
    Invalid(&'static str),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "invalid manifest JSON: {error}"),
            Self::Invalid(reason) => write!(f, "invalid manifest: {reason}"),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

fn require(condition: bool, reason: &'static str) -> Result<(), ManifestError> {
    if condition {
        Ok(())
    } else {
        Err(ManifestError::Invalid(reason))
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
