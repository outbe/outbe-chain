//! Disk-backed authority for one finalized Tribute population.
//!
//! The input population may be arbitrarily larger than RAM. Therefore this module:
//!
//! - keeps only one bounded sort run in memory
//! - merges runs with bounded fan-in
//! - publishes the immutable inventory header only after the CE root, exact
//!   count, and nominal total have all closed

use std::{
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    body_commitment, BoundedTributePartitionVerifier, Commitment, TributePartitionExpectationV1,
    TributePartitionRetentionStatsV1, TributePartitionWorkConfig, TributeProofArchiveV1,
    WwdEntityId, ACTIVE_COMMITMENT_SCHEME,
};
use outbe_ocomp_protocol::input::CheckpointIdentityV1;
use outbe_oracle::MAX_OCOMP_REFERENCE_ISOS;
use outbe_primitives::time::WorldwideDay;
use sha3::{Digest, Keccak256};
use thiserror::Error;

mod filesystem;
mod header;
mod open;
mod owner_runs;

use filesystem::*;
use header::{decode_header, encode_header};
pub use open::{open_sealed_inventory, open_sealed_inventory_observing};
pub use owner_runs::OwnerBatchReader;
use owner_runs::{
    install_owner_file, merge_run_group, verify_owner_file, OwnerRunGroup, OwnerRunPosition,
    OwnerRunWriter,
};

const DIRECTORY_MODE: u32 = 0o750;
const FILE_MODE: u32 = 0o640;
const HEADER_MAGIC: [u8; 8] = *b"OUTBTIH1";
const RUN_MAGIC: [u8; 8] = *b"OUTBTIR1";
const BODY_MAGIC: [u8; 8] = *b"OUTBTIB1";
const HEADER_FILE: &str = "inventory.header";
const OWNERS_FILE: &str = "owners.sorted";
const BODIES_FILE: &str = "tributes.spool";
pub const SOURCE_PROOF_ARCHIVE_DIRECTORY: &str = "source-proof-archive-v1";
const ISOS_FILE: &str = "reference-isos.bitmap";
const LOCK_FILE: &str = "inventory.lock";
const BUILD_DIRECTORY: &str = "building";
const OWNER_BYTES: usize = 20;
const ISO_BITMAP_BYTES: usize = 8_192;
const RUN_HEADER_BYTES: u64 = 16;
const BODY_HEADER_BYTES: u64 = 20;
// Work heartbeat cadence only. It does not cap the inventory population.
const INVENTORY_PROGRESS_RECORD_HEARTBEAT: u64 = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeInventorySubjectV1 {
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
    pub checkpoint: CheckpointIdentityV1,
    pub worldwide_day: WorldwideDay,
    pub sealed_tribute_collection_root: B256,
    pub expected_tribute_count: u32,
    pub expected_nominal_total: U256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeInventoryWorkConfig {
    pub owners_per_run: usize,
    pub merge_fan_in: usize,
    pub root_verifier: TributePartitionWorkConfig,
}

impl Default for TributeInventoryWorkConfig {
    fn default() -> Self {
        Self {
            owners_per_run: 4_096,
            merge_fan_in: 16,
            root_verifier: TributePartitionWorkConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeInventoryRecordV1 {
    pub tribute_id: WwdEntityId,
    pub commitment: Commitment,
    pub owner: Address,
    pub reference_iso: u16,
    pub nominal_amount_minor: U256,
    pub canonical_body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeInventoryRetentionStatsV1 {
    pub current_owner_records: usize,
    pub peak_owner_records: usize,
    pub configured_owner_record_bound: usize,
    pub root_verifier: TributePartitionRetentionStatsV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InventoryHeaderV1 {
    subject: TributeInventorySubjectV1,
    unique_owner_count: u64,
    owner_file_digest: B256,
    iso_bitmap_digest: B256,
    body_file_digest: B256,
    exact_body_bytes: u64,
}

pub struct TributeInventoryBuilder {
    root: PathBuf,
    build_root: PathBuf,
    subject: TributeInventorySubjectV1,
    work: TributeInventoryWorkConfig,
    root_verifier: Option<BoundedTributePartitionVerifier>,
    body_writer: Option<BodySpoolWriter>,
    owner_buffer: Vec<Address>,
    peak_owner_records: usize,
    run_count: u64,
    tribute_count: u32,
    nominal_total: U256,
    previous_tribute_id: Option<WwdEntityId>,
    iso_bitmap: Box<[u8; ISO_BITMAP_BYTES]>,
    _lock: InventoryLock,
}

// Keep the authenticated header and its ISO bitmap in one immutable metadata value.
struct InventoryMetadata {
    header: InventoryHeaderV1,
    isos: Box<[u8; ISO_BITMAP_BYTES]>,
}

pub struct SealedTributeInventory {
    root: PathBuf,
    metadata: InventoryMetadata,
    _lock: InventoryLock,
}

pub struct TributeBodySpoolReader {
    file: File,
    remaining: u32,
    exact_body_bytes: u64,
    consumed_body_bytes: u64,
}

impl TributeInventoryBuilder {
    pub fn create(
        root: impl AsRef<Path>,
        subject: TributeInventorySubjectV1,
        work: TributeInventoryWorkConfig,
    ) -> Result<Self, TributeInventoryError> {
        if work.owners_per_run == 0 || work.merge_fan_in < 2 {
            return Err(TributeInventoryError::InvalidWorkConfig);
        }
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let lock = InventoryLock::acquire(&root)?;
        if path_exists(&root.join(HEADER_FILE))? {
            return Err(TributeInventoryError::AlreadySealed);
        }
        recover_unsealed_inventory(&root)?;
        let build_root = root.join(BUILD_DIRECTORY);
        remove_owned_build_directory(&build_root)?;
        fs::create_dir(&build_root)
            .map_err(|source| io_error("create inventory build directory", &build_root, source))?;
        fs::set_permissions(&build_root, fs::Permissions::from_mode(DIRECTORY_MODE))
            .map_err(|source| io_error("set inventory build permissions", &build_root, source))?;
        sync_directory(&root)?;
        let root_verifier = BoundedTributePartitionVerifier::create(
            build_root.join("root-verifier"),
            TributePartitionExpectationV1 {
                day: subject.worldwide_day,
                exact_leaf_count: subject.expected_tribute_count,
                expected_collection_root: subject.sealed_tribute_collection_root,
                commitment_scheme: ACTIVE_COMMITMENT_SCHEME,
            },
            work.root_verifier,
        )?;
        let mut iso_bitmap = Box::new([0_u8; ISO_BITMAP_BYTES]);
        set_iso(&mut iso_bitmap, 840);
        let body_writer = BodySpoolWriter::create(build_root.join(BODIES_FILE))?;
        Ok(Self {
            root,
            build_root,
            subject,
            work,
            root_verifier: Some(root_verifier),
            body_writer: Some(body_writer),
            owner_buffer: Vec::with_capacity(work.owners_per_run),
            peak_owner_records: 0,
            run_count: 0,
            tribute_count: 0,
            nominal_total: U256::ZERO,
            previous_tribute_id: None,
            iso_bitmap,
            _lock: lock,
        })
    }

    pub fn push(&mut self, record: TributeInventoryRecordV1) -> Result<(), TributeInventoryError> {
        if record.tribute_id.worldwide_day() != self.subject.worldwide_day {
            return Err(TributeInventoryError::Authority("Tribute worldwide day"));
        }
        if self
            .previous_tribute_id
            .is_some_and(|previous| previous >= record.tribute_id)
        {
            return Err(TributeInventoryError::Authority(
                "canonical Tribute stream order",
            ));
        }
        let decoded = outbe_tribute::record::decode_canonical(&record.canonical_body)
            .map_err(|_| TributeInventoryError::Authority("canonical Tribute body"))?;
        if decoded.tribute_id != record.tribute_id
            || decoded.owner != record.owner
            || decoded.worldwide_day != self.subject.worldwide_day
            || decoded.reference_currency != record.reference_iso
        {
            return Err(TributeInventoryError::Authority(
                "canonical Tribute body fields",
            ));
        }
        let body_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            decoded
                .stored_body()
                .map_err(|_| TributeInventoryError::Authority("canonical Tribute body schema"))?
                .schema_version(),
            record.tribute_id,
            &record.canonical_body,
        )
        .map_err(|_| TributeInventoryError::Authority("canonical Tribute body commitment"))?;
        if body_commitment != record.commitment {
            return Err(TributeInventoryError::Authority(
                "canonical Tribute body commitment",
            ));
        }
        if decoded.calculation_view()?.nominal_amount_minor != record.nominal_amount_minor {
            return Err(TributeInventoryError::Authority(
                "private Tribute nominal amount",
            ));
        }
        self.root_verifier
            .as_mut()
            .expect("root verifier exists until inventory finish")
            .push(record.tribute_id, record.commitment)?;
        self.body_writer
            .as_mut()
            .expect("body writer exists until inventory finish")
            .write(&record.canonical_body)?;
        self.tribute_count = self
            .tribute_count
            .checked_add(1)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        self.nominal_total = self
            .nominal_total
            .checked_add(record.nominal_amount_minor)
            .ok_or(TributeInventoryError::NominalTotalOverflow)?;
        set_iso(&mut self.iso_bitmap, record.reference_iso);
        self.owner_buffer.push(record.owner);
        self.peak_owner_records = self.peak_owner_records.max(self.owner_buffer.len());
        self.previous_tribute_id = Some(record.tribute_id);
        if self.owner_buffer.len() == self.work.owners_per_run {
            self.flush_owner_run()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn retention_stats(&self) -> TributeInventoryRetentionStatsV1 {
        TributeInventoryRetentionStatsV1 {
            current_owner_records: self.owner_buffer.len(),
            peak_owner_records: self.peak_owner_records,
            configured_owner_record_bound: self.work.owners_per_run,
            root_verifier: self
                .root_verifier
                .as_ref()
                .expect("root verifier exists until inventory finish")
                .retention_stats(),
        }
    }

    pub fn finish(self) -> Result<SealedTributeInventory, TributeInventoryError> {
        self.finish_observing(|| {})
    }

    pub fn finish_observing(
        mut self,
        on_progress: impl Fn(),
    ) -> Result<SealedTributeInventory, TributeInventoryError> {
        if self.tribute_count != self.subject.expected_tribute_count {
            return Err(TributeInventoryError::CountMismatch {
                expected: self.subject.expected_tribute_count,
                actual: self.tribute_count,
            });
        }
        if self.nominal_total != self.subject.expected_nominal_total {
            return Err(TributeInventoryError::NominalTotalMismatch {
                expected: self.subject.expected_nominal_total,
                actual: self.nominal_total,
            });
        }
        let reference_iso_count = self
            .iso_bitmap
            .iter()
            .try_fold(0_usize, |total, byte| {
                total.checked_add(byte.count_ones() as usize)
            })
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        if reference_iso_count > MAX_OCOMP_REFERENCE_ISOS {
            return Err(TributeInventoryError::ReferenceIsoCountOutsideProtocol {
                limit: MAX_OCOMP_REFERENCE_ISOS,
                actual: reference_iso_count,
            });
        }
        self.install_source_proofs(&on_progress)?;
        let body_summary = self
            .body_writer
            .take()
            .expect("body writer exists until inventory finish")
            .finish()?;
        self.flush_owner_run()?;
        on_progress();
        let final_run = self.merge_owner_runs(&on_progress)?;
        let owners_tmp = self.root.join(format!("{OWNERS_FILE}.tmp"));
        let unique_owner_count =
            install_owner_file(final_run.as_deref(), &owners_tmp, &on_progress)?;
        let owner_file_digest = digest_file_observing(&owners_tmp, &on_progress)?;
        let bodies_tmp = self.root.join(format!("{BODIES_FILE}.tmp"));
        fs::rename(self.build_root.join(BODIES_FILE), &bodies_tmp)
            .map_err(|source| io_error("stage Tribute body spool", &bodies_tmp, source))?;
        let body_file_digest = digest_file_observing(&bodies_tmp, &on_progress)?;
        let isos_tmp = self.root.join(format!("{ISOS_FILE}.tmp"));
        persist_new(&isos_tmp, &self.iso_bitmap[..])?;
        let iso_bitmap_digest = B256::from_slice(&Keccak256::digest(&self.iso_bitmap[..]));
        fs::rename(&owners_tmp, self.root.join(OWNERS_FILE))
            .map_err(|source| io_error("install owner inventory", &owners_tmp, source))?;
        fs::rename(&isos_tmp, self.root.join(ISOS_FILE))
            .map_err(|source| io_error("install ISO inventory", &isos_tmp, source))?;
        fs::rename(&bodies_tmp, self.root.join(BODIES_FILE))
            .map_err(|source| io_error("install Tribute body spool", &bodies_tmp, source))?;
        sync_directory(&self.root)?;
        let header = InventoryHeaderV1 {
            subject: self.subject,
            unique_owner_count,
            owner_file_digest,
            iso_bitmap_digest,
            body_file_digest,
            exact_body_bytes: body_summary.exact_body_bytes,
        };
        persist_atomic(
            &self.root,
            &self.root.join(HEADER_FILE),
            &encode_header(&header),
        )?;
        remove_owned_build_directory(&self.build_root)?;
        sync_directory(&self.root)?;
        on_progress();
        Ok(SealedTributeInventory {
            root: self.root,
            metadata: InventoryMetadata {
                header,
                isos: self.iso_bitmap,
            },
            _lock: self._lock,
        })
    }

    fn install_source_proofs(
        &mut self,
        on_progress: &impl Fn(),
    ) -> Result<(), TributeInventoryError> {
        let proof_archive = self
            .root_verifier
            .take()
            .expect("root verifier exists until inventory finish")
            .finish_with_archive(on_progress)?;
        let proof_archive_path = proof_archive.path().to_path_buf();
        drop(proof_archive);
        fs::rename(
            &proof_archive_path,
            self.root.join(SOURCE_PROOF_ARCHIVE_DIRECTORY),
        )
        .map_err(|source| {
            io_error(
                "install Tribute source proof archive",
                &proof_archive_path,
                source,
            )
        })
    }

    fn flush_owner_run(&mut self) -> Result<(), TributeInventoryError> {
        if self.owner_buffer.is_empty() {
            return Ok(());
        }
        self.owner_buffer.sort_unstable();
        self.owner_buffer.dedup();
        let path = run_path(&self.build_root, 0, self.run_count);
        let mut writer = OwnerRunWriter::create(path)?;
        for owner in &self.owner_buffer {
            writer.write(*owner)?;
        }
        writer.finish()?;
        self.owner_buffer.clear();
        self.run_count = self
            .run_count
            .checked_add(1)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        Ok(())
    }

    fn merge_owner_runs(
        &self,
        on_progress: &impl Fn(),
    ) -> Result<Option<PathBuf>, TributeInventoryError> {
        if self.run_count == 0 {
            return Ok(None);
        }
        let fan_in = u64::try_from(self.work.merge_fan_in)
            .map_err(|_| TributeInventoryError::IntegerOverflow)?;
        let mut pass = 0_u32;
        let mut run_count = self.run_count;
        while run_count > 1 {
            let output_pass = pass
                .checked_add(1)
                .ok_or(TributeInventoryError::IntegerOverflow)?;
            let next_count = run_count
                .checked_add(fan_in - 1)
                .ok_or(TributeInventoryError::IntegerOverflow)?
                / fan_in;
            for output_index in 0..next_count {
                let start = output_index
                    .checked_mul(fan_in)
                    .ok_or(TributeInventoryError::IntegerOverflow)?;
                let end = start
                    .checked_add(fan_in)
                    .ok_or(TributeInventoryError::IntegerOverflow)?
                    .min(run_count);
                merge_run_group(
                    &self.build_root,
                    OwnerRunGroup {
                        pass,
                        range: start..end,
                    },
                    OwnerRunPosition {
                        pass: output_pass,
                        index: output_index,
                    },
                    on_progress,
                )?;
            }
            for index in 0..run_count {
                let path = run_path(&self.build_root, pass, index);
                fs::remove_file(&path)
                    .map_err(|source| io_error("remove merged owner run", &path, source))?;
            }
            sync_directory(&self.build_root)?;
            on_progress();
            pass = output_pass;
            run_count = next_count;
        }
        Ok(Some(run_path(&self.build_root, pass, 0)))
    }
}

impl SealedTributeInventory {
    #[must_use]
    pub const fn unique_owner_count(&self) -> u64 {
        self.metadata.header.unique_owner_count
    }

    #[must_use]
    pub fn authority_digest(&self) -> B256 {
        B256::from_slice(&Keccak256::digest(encode_header(&self.metadata.header)))
    }

    pub fn owner_batches(&self) -> Result<OwnerBatchReader, TributeInventoryError> {
        OwnerBatchReader::open(
            self.root.join(OWNERS_FILE),
            self.metadata.header.unique_owner_count,
        )
    }

    pub fn reference_isos(&self) -> Vec<u16> {
        (u16::MIN..=u16::MAX)
            .filter(|iso| contains_iso(&self.metadata.isos, *iso))
            .collect()
    }

    pub fn tribute_bodies(&self) -> Result<TributeBodySpoolReader, TributeInventoryError> {
        TributeBodySpoolReader::open(
            self.root.join(BODIES_FILE),
            self.metadata.header.subject.expected_tribute_count,
            self.metadata.header.exact_body_bytes,
        )
    }

    pub fn source_proofs(&self) -> Result<TributeProofArchiveV1, TributeInventoryError> {
        Ok(outbe_compressed_entities::open_tribute_proof_archive(
            self.root.join(SOURCE_PROOF_ARCHIVE_DIRECTORY),
            TributePartitionExpectationV1 {
                day: self.metadata.header.subject.worldwide_day,
                exact_leaf_count: self.metadata.header.subject.expected_tribute_count,
                expected_collection_root: self
                    .metadata
                    .header
                    .subject
                    .sealed_tribute_collection_root,
                commitment_scheme: ACTIVE_COMMITMENT_SCHEME,
            },
        )?)
    }
}

impl TributeBodySpoolReader {
    fn open(
        path: PathBuf,
        expected_count: u32,
        expected_body_bytes: u64,
    ) -> Result<Self, TributeInventoryError> {
        let mut file = open_regular_readonly(&path)?;
        let (remaining, exact_body_bytes) = read_body_header(&mut file, &path)?;
        if remaining != expected_count || exact_body_bytes != expected_body_bytes {
            return Err(TributeInventoryError::Corrupt("Tribute body spool header"));
        }
        Ok(Self {
            file,
            remaining,
            exact_body_bytes,
            consumed_body_bytes: 0,
        })
    }

    pub fn next_body(
        &mut self,
        max_body_bytes: usize,
    ) -> Result<Option<Vec<u8>>, TributeInventoryError> {
        if self.remaining == 0 {
            if self.consumed_body_bytes != self.exact_body_bytes {
                return Err(TributeInventoryError::Corrupt(
                    "Tribute body spool byte count",
                ));
            }
            return Ok(None);
        }
        let mut length = [0_u8; 4];
        self.file.read_exact(&mut length).map_err(|source| {
            io_error("read Tribute body length", Path::new(BODIES_FILE), source)
        })?;
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| TributeInventoryError::IntegerOverflow)?;
        if length == 0 || length > max_body_bytes {
            return Err(TributeInventoryError::BodyOutsideBound {
                limit: max_body_bytes,
                actual: length,
            });
        }
        let mut body = vec![0_u8; length];
        self.file
            .read_exact(&mut body)
            .map_err(|source| io_error("read Tribute body", Path::new(BODIES_FILE), source))?;
        self.remaining -= 1;
        self.consumed_body_bytes = self
            .consumed_body_bytes
            .checked_add(u64::try_from(length).map_err(|_| TributeInventoryError::IntegerOverflow)?)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        Ok(Some(body))
    }
}

struct BodySpoolWriter {
    path: PathBuf,
    file: File,
    count: u32,
    exact_body_bytes: u64,
}

struct BodySpoolSummary {
    exact_body_bytes: u64,
}

impl BodySpoolWriter {
    fn create(path: PathBuf) -> Result<Self, TributeInventoryError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&path)
            .map_err(|source| io_error("create Tribute body spool", &path, source))?;
        file.write_all(&BODY_MAGIC)
            .and_then(|()| file.write_all(&0_u32.to_be_bytes()))
            .and_then(|()| file.write_all(&0_u64.to_be_bytes()))
            .map_err(|source| io_error("write Tribute body spool header", &path, source))?;
        Ok(Self {
            path,
            file,
            count: 0,
            exact_body_bytes: 0,
        })
    }

    fn write(&mut self, body: &[u8]) -> Result<(), TributeInventoryError> {
        let length =
            u32::try_from(body.len()).map_err(|_| TributeInventoryError::BodyOutsideBound {
                limit: u32::MAX as usize,
                actual: body.len(),
            })?;
        if length == 0 {
            return Err(TributeInventoryError::BodyOutsideBound {
                limit: u32::MAX as usize,
                actual: 0,
            });
        }
        self.file
            .write_all(&length.to_be_bytes())
            .and_then(|()| self.file.write_all(body))
            .map_err(|source| io_error("write Tribute body spool", &self.path, source))?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        self.exact_body_bytes = self
            .exact_body_bytes
            .checked_add(u64::from(length))
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        Ok(())
    }

    fn finish(mut self) -> Result<BodySpoolSummary, TributeInventoryError> {
        self.file
            .seek(SeekFrom::Start(8))
            .and_then(|_| self.file.write_all(&self.count.to_be_bytes()))
            .and_then(|()| self.file.write_all(&self.exact_body_bytes.to_be_bytes()))
            .and_then(|()| self.file.sync_all())
            .map_err(|source| io_error("finish Tribute body spool", &self.path, source))?;
        Ok(BodySpoolSummary {
            exact_body_bytes: self.exact_body_bytes,
        })
    }
}

fn set_iso(bitmap: &mut [u8; ISO_BITMAP_BYTES], iso: u16) {
    let index = usize::from(iso);
    bitmap[index / 8] |= 1 << (index % 8);
}

fn contains_iso(bitmap: &[u8; ISO_BITMAP_BYTES], iso: u16) -> bool {
    let index = usize::from(iso);
    bitmap[index / 8] & (1 << (index % 8)) != 0
}

fn run_path(root: &Path, pass: u32, index: u64) -> PathBuf {
    root.join(format!("owners-{pass:010}-{index:020}.run"))
}

fn read_run_header(file: &mut File, path: &Path) -> Result<u64, TributeInventoryError> {
    let mut header = [0_u8; RUN_HEADER_BYTES as usize];
    file.read_exact(&mut header)
        .map_err(|source| io_error("read owner run header", path, source))?;
    if header[..8] != RUN_MAGIC {
        return Err(TributeInventoryError::Corrupt("owner run magic"));
    }
    let count = u64::from_be_bytes(header[8..].try_into().expect("fixed run header"));
    let expected_len = RUN_HEADER_BYTES
        .checked_add(
            count
                .checked_mul(OWNER_BYTES as u64)
                .ok_or(TributeInventoryError::IntegerOverflow)?,
        )
        .ok_or(TributeInventoryError::IntegerOverflow)?;
    let actual_len = file
        .metadata()
        .map_err(|source| io_error("stat owner run", path, source))?
        .len();
    if actual_len != expected_len {
        return Err(TributeInventoryError::Corrupt("owner run length"));
    }
    Ok(count)
}

fn read_body_header(file: &mut File, path: &Path) -> Result<(u32, u64), TributeInventoryError> {
    let mut header = [0_u8; BODY_HEADER_BYTES as usize];
    file.read_exact(&mut header)
        .map_err(|source| io_error("read Tribute body spool header", path, source))?;
    if header[..8] != BODY_MAGIC {
        return Err(TributeInventoryError::Corrupt("Tribute body spool magic"));
    }
    let count = u32::from_be_bytes(header[8..12].try_into().expect("fixed body header"));
    let exact_body_bytes =
        u64::from_be_bytes(header[12..20].try_into().expect("fixed body header"));
    Ok((count, exact_body_bytes))
}

fn verify_body_spool(
    path: &Path,
    expected_count: u32,
    expected_body_bytes: u64,
    on_progress: &impl Fn(),
) -> Result<(), TributeInventoryError> {
    let mut file = open_regular_readonly(path)?;
    let (count, exact_body_bytes) = read_body_header(&mut file, path)?;
    if count != expected_count || exact_body_bytes != expected_body_bytes {
        return Err(TributeInventoryError::Corrupt("Tribute body spool header"));
    }
    let mut observed_body_bytes = 0_u64;
    for index in 0..count {
        let mut length = [0_u8; 4];
        file.read_exact(&mut length)
            .map_err(|source| io_error("read Tribute body length", path, source))?;
        let length = u32::from_be_bytes(length);
        if length == 0 {
            return Err(TributeInventoryError::Corrupt("empty Tribute body"));
        }
        observed_body_bytes = observed_body_bytes
            .checked_add(u64::from(length))
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        file.seek(SeekFrom::Current(i64::from(length)))
            .map_err(|source| io_error("scan Tribute body spool", path, source))?;
        if u64::from(index + 1) % INVENTORY_PROGRESS_RECORD_HEARTBEAT == 0 {
            on_progress();
        }
    }
    let position = file
        .stream_position()
        .map_err(|source| io_error("close Tribute body spool", path, source))?;
    let length = file
        .metadata()
        .map_err(|source| io_error("stat Tribute body spool", path, source))?
        .len();
    if observed_body_bytes != expected_body_bytes || position != length {
        return Err(TributeInventoryError::Corrupt("Tribute body spool closure"));
    }
    Ok(())
}

fn read_owner(file: &mut File) -> Result<Address, TributeInventoryError> {
    let mut bytes = [0_u8; OWNER_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|source| io_error("read owner inventory", Path::new(OWNERS_FILE), source))?;
    Ok(Address::from(bytes))
}

#[derive(Debug, Error)]
pub enum TributeInventoryError {
    #[error("private Tribute amount read failed: {0}")]
    PrivateTribute(#[from] outbe_tee::TransportError),
    #[error("invalid Tribute inventory work configuration")]
    InvalidWorkConfig,
    #[error("Tribute inventory is already sealed")]
    AlreadySealed,
    #[error("Tribute inventory is locked by another writer")]
    Locked,
    #[error("unsafe Tribute inventory path: {0}")]
    UnsafePath(PathBuf),
    #[error("Tribute inventory authority mismatch: {0}")]
    Authority(&'static str),
    #[error("corrupt Tribute inventory: {0}")]
    Corrupt(&'static str),
    #[error("Tribute count mismatch: expected {expected}, got {actual}")]
    CountMismatch { expected: u32, actual: u32 },
    #[error("Tribute nominal total mismatch: expected {expected}, got {actual}")]
    NominalTotalMismatch { expected: U256, actual: U256 },
    #[error("Tribute inventory nominal total overflow")]
    NominalTotalOverflow,
    #[error("reference ISO count {actual} exceeds existing OCOMP protocol bound {limit}")]
    ReferenceIsoCountOutsideProtocol { limit: usize, actual: usize },
    #[error("canonical Tribute body has {actual} bytes outside per-body bound {limit}")]
    BodyOutsideBound { limit: usize, actual: usize },
    #[error("Tribute inventory integer overflow")]
    IntegerOverflow,
    #[error(transparent)]
    Partition(#[from] outbe_compressed_entities::TributePartitionReconstructionError),
    #[error("Tribute inventory I/O failed during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> TributeInventoryError {
    TributeInventoryError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

use std::os::unix::fs::PermissionsExt;
