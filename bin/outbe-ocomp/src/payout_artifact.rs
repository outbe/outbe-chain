//! Payout handoff artifact: the day's dense eligible-contributor list, written
//! at finalization so the payout sender can read the day back without
//! reopening the audit machinery. The sender re-derives the merkle root from
//! it, so a torn or stale file fails closed.

use std::fs;
use std::io::{BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use alloy_primitives::{B256, U256};
use outbe_intex::{
    payout::{
        decode_contributor_leaf, encode_contributor_leaf, ContributorLeafData,
        CONTRIBUTOR_LEAF_BYTES,
    },
    CertifiedContributorGenerationProjection,
};
use outbe_ocomp_protocol::{ListKind, ProtocolError, StreamingOrderedListRoot};
use thiserror::Error;

use crate::lysis_plan_audit::LocalLysisPlanAuditV1;
use crate::lysis_result_catalog::{
    ExactLysisResultCatalogCursorV1, LysisResultCatalogError, LysisResultCatalogStepV1,
};

pub const CONTRIBUTOR_PAYOUT_ARTIFACT_FILE: &str = "contributor-payout-v1.bin";

const TEMP_SUFFIX: &str = ".tmp";

#[derive(Debug, Error)]
pub enum PayoutArtifactError {
    #[error("payout artifact catalog: {0}")]
    Catalog(#[from] LysisResultCatalogError),
    #[error("payout artifact {context}: {source}")]
    Io {
        context: &'static str,
        source: std::io::Error,
    },
    #[error("payout artifact record count overflow")]
    CountOverflow,
    #[error("result catalog ended before its completion marker")]
    IncompleteCatalog,
    #[error("invalid certified contributor generation: {0}")]
    InvalidCertifiedGeneration(&'static str),
    #[error("payout artifact is not a regular file")]
    NotRegularFile,
    #[error("payout artifact length mismatch: expected {expected} bytes, found {actual}")]
    LengthMismatch { expected: u64, actual: u64 },
    #[error("payout artifact nominal total overflows U256")]
    NominalOverflow,
    #[error("payout artifact root mismatch: expected {expected}, found {actual}")]
    RootMismatch { expected: B256, actual: B256 },
    #[error("payout artifact nominal total mismatch: expected {expected}, found {actual}")]
    TotalMismatch { expected: U256, actual: U256 },
    #[error("payout artifact changed while being read")]
    SourceChanged,
    #[error("payout artifact ordered-list commitment: {0}")]
    Protocol(#[from] ProtocolError),
}

fn io_error(context: &'static str, source: std::io::Error) -> PayoutArtifactError {
    PayoutArtifactError::Io { context, source }
}

/// Observed native contributor population matching the supplied certification.
/// Canonical day/job selection remains the caller's responsibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedPayoutArtifactV1 {
    pub contributor_count: u32,
    pub contributor_root: B256,
    pub eligible_nominal_total: U256,
}

/// Verifies the public payout file without opening intermediate catalogs or writers.
///
/// The caller obtains certification from verified canonical state and derives the
/// canonical job path separately: the certified projection does not contain a JobId.
/// Reads retain one 84-byte record and the native fixed-size ordered-list frontier.
pub fn verify_contributor_payout_artifact(
    path: &Path,
    certified: &CertifiedContributorGenerationProjection,
) -> Result<VerifiedPayoutArtifactV1, PayoutArtifactError> {
    if certified.contributor_root.is_zero() {
        return Err(PayoutArtifactError::InvalidCertifiedGeneration(
            "missing certified root",
        ));
    }
    if !matches!(certified.series_version, 1 | 2) {
        return Err(PayoutArtifactError::InvalidCertifiedGeneration(
            "invalid series version",
        ));
    }
    if (certified.contributor_count == 0) != certified.eligible_nominal_total.is_zero() {
        return Err(PayoutArtifactError::InvalidCertifiedGeneration(
            "count/total presence mismatch",
        ));
    }
    // NONBLOCK prevents a replaced FIFO/device from blocking before the regular
    // file check; it does not change regular-file read behavior.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error("open for verification", source))?;
    let initial = file
        .metadata()
        .map_err(|source| io_error("stat for verification", source))?;
    if !initial.is_file() {
        return Err(PayoutArtifactError::NotRegularFile);
    }
    let expected = u64::from(certified.contributor_count)
        .checked_mul(CONTRIBUTOR_LEAF_BYTES as u64)
        .ok_or(PayoutArtifactError::CountOverflow)?;
    if initial.len() != expected {
        return Err(PayoutArtifactError::LengthMismatch {
            expected,
            actual: initial.len(),
        });
    }
    verify_payout_source_unchanged(path, &file, &initial)?;
    let mut root =
        StreamingOrderedListRoot::new(ListKind::ContributorActions, certified.contributor_count)?;
    let mut total = U256::ZERO;
    let mut record = [0; CONTRIBUTOR_LEAF_BYTES];
    for _ in 0..certified.contributor_count {
        file.read_exact(&mut record).map_err(|source| {
            if source.kind() == std::io::ErrorKind::UnexpectedEof {
                PayoutArtifactError::SourceChanged
            } else {
                io_error("read contributor record", source)
            }
        })?;
        let leaf = decode_contributor_leaf(&record);
        total = total
            .checked_add(leaf.nominal)
            .ok_or(PayoutArtifactError::NominalOverflow)?;
        root.push(&record, CONTRIBUTOR_LEAF_BYTES)?;
    }
    if file
        .read(&mut [0; 1])
        .map_err(|source| io_error("check artifact EOF", source))?
        != 0
    {
        return Err(PayoutArtifactError::SourceChanged);
    }
    let contributor_root = root.finish()?;
    verify_payout_source_unchanged(path, &file, &initial)?;
    if contributor_root != certified.contributor_root {
        return Err(PayoutArtifactError::RootMismatch {
            expected: certified.contributor_root,
            actual: contributor_root,
        });
    }
    if total != certified.eligible_nominal_total {
        return Err(PayoutArtifactError::TotalMismatch {
            expected: certified.eligible_nominal_total,
            actual: total,
        });
    }
    Ok(VerifiedPayoutArtifactV1 {
        contributor_count: certified.contributor_count,
        contributor_root,
        eligible_nominal_total: total,
    })
}

fn verify_payout_source_unchanged(
    path: &Path,
    file: &fs::File,
    initial: &fs::Metadata,
) -> Result<(), PayoutArtifactError> {
    let opened = file
        .metadata()
        .map_err(|source| io_error("restat payout descriptor", source))?;
    let current = fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            PayoutArtifactError::SourceChanged
        } else {
            io_error("restat payout path", source)
        }
    })?;
    let unchanged = |metadata: &fs::Metadata| {
        metadata.is_file()
            && metadata.dev() == initial.dev()
            && metadata.ino() == initial.ino()
            && metadata.len() == initial.len()
            && metadata.mtime() == initial.mtime()
            && metadata.mtime_nsec() == initial.mtime_nsec()
            && metadata.ctime() == initial.ctime()
            && metadata.ctime_nsec() == initial.ctime_nsec()
    };
    if !unchanged(&opened) || !unchanged(&current) {
        return Err(PayoutArtifactError::SourceChanged);
    }
    Ok(())
}

/// Streams records into a temp file; the final name appears only on `commit`.
struct PayoutArtifactWriter {
    file: BufWriter<fs::File>,
    temp: PathBuf,
    target: PathBuf,
    count: u32,
}

impl PayoutArtifactWriter {
    fn open(job_root: &Path) -> Result<Self, PayoutArtifactError> {
        let target = job_root.join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE);
        let temp = job_root.join(format!("{CONTRIBUTOR_PAYOUT_ARTIFACT_FILE}{TEMP_SUFFIX}"));
        match fs::remove_file(&temp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("clear stale temp", error)),
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| io_error("create temp", error))?;
        Ok(Self {
            file: BufWriter::new(file),
            temp,
            target,
            count: 0,
        })
    }

    fn push(&mut self, leaf: &ContributorLeafData) -> Result<(), PayoutArtifactError> {
        self.file
            .write_all(&encode_contributor_leaf(leaf))
            .map_err(|error| io_error("append record", error))?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or(PayoutArtifactError::CountOverflow)?;
        Ok(())
    }

    fn commit(self) -> Result<u32, PayoutArtifactError> {
        let Self {
            file,
            temp,
            target,
            count,
        } = self;
        let file = file
            .into_inner()
            .map_err(|error| io_error("flush records", error.into_error()))?;
        file.sync_all()
            .map_err(|error| io_error("sync records", error))?;
        fs::rename(&temp, &target).map_err(|error| io_error("install artifact", error))?;
        let directory = target.parent().expect("artifact path has a parent");
        fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_error("sync directory", error))?;
        Ok(count)
    }
}

/// Streams every eligible contributor of the finalized day into the job
/// directory, in tree order, and returns the record count.
pub fn write_contributor_payout_artifact(
    audit: &LocalLysisPlanAuditV1<'_>,
    job_root: &Path,
) -> Result<u32, PayoutArtifactError> {
    let mut writer = PayoutArtifactWriter::open(job_root)?;
    let mut complete = false;
    for step in ExactLysisResultCatalogCursorV1::open(audit)? {
        match step? {
            LysisResultCatalogStepV1::Chunk(chunk) => {
                for action in &chunk.chunk().ordered_eligible_contributors {
                    writer.push(&ContributorLeafData {
                        owner: action.owner,
                        source_tribute_id: U256::from_be_bytes(action.source_tribute_id.0),
                        nominal: action.nominal_amount_minor,
                    })?;
                }
            }
            LysisResultCatalogStepV1::Complete => complete = true,
            _ => {}
        }
    }
    if !complete {
        return Err(PayoutArtifactError::IncompleteCatalog);
    }
    writer.commit()
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};

    use super::*;

    fn leaf(index: u8) -> ContributorLeafData {
        ContributorLeafData {
            owner: Address::repeat_byte(index),
            source_tribute_id: U256::from(index),
            nominal: U256::from(u64::from(index) + 1),
        }
    }

    #[test]
    fn commit_installs_the_exact_concatenation() {
        let dir = tempfile::tempdir().unwrap();
        let leaves: Vec<_> = (0..3).map(leaf).collect();

        let mut writer = PayoutArtifactWriter::open(dir.path()).unwrap();
        for entry in &leaves {
            writer.push(entry).unwrap();
        }
        assert!(!dir.path().join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE).exists());
        assert_eq!(writer.commit().unwrap(), 3);

        let bytes = fs::read(dir.path().join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE)).unwrap();
        let expected: Vec<u8> = leaves.iter().flat_map(encode_contributor_leaf).collect();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn an_uncommitted_writer_never_installs_the_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = PayoutArtifactWriter::open(dir.path()).unwrap();
        writer.push(&leaf(7)).unwrap();
        drop(writer);
        assert!(!dir.path().join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE).exists());
    }

    #[test]
    fn payout_source_identity_rejects_same_size_rewrite_and_path_replacement() {
        use std::fs::FileTimes;
        use std::time::{Duration, SystemTime};

        for replace in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE);
            fs::write(&path, encode_contributor_leaf(&leaf(1))).unwrap();
            let file = fs::File::open(&path).unwrap();
            file.set_times(
                FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
            let initial = file.metadata().unwrap();
            if replace {
                let replacement = dir.path().join("replacement");
                fs::write(&replacement, encode_contributor_leaf(&leaf(1))).unwrap();
                fs::rename(replacement, &path).unwrap();
            } else {
                fs::write(&path, encode_contributor_leaf(&leaf(2))).unwrap();
            }
            assert!(matches!(
                verify_payout_source_unchanged(&path, &file, &initial),
                Err(PayoutArtifactError::SourceChanged)
            ));
        }
    }
}
