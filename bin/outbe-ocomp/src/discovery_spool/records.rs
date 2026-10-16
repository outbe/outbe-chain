use std::{
    fs::{self, ReadDir},
    path::{Path, PathBuf},
};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{
    control::{FinalizedJobSpecV1, SnapshotExportCommittedV1},
    input::CheckpointIdentityV1,
    intent::JobIntentV1,
    profile::ProtocolBundleV1,
    ProtocolError,
};

use crate::discovery_control::{observation_id, DiscoveryAckRefV1, DiscoveryOfferRefV1};
use crate::export_receipt::VerifiedExportReceipt;

use super::{
    checkpoint::{
        persist_immutable_atomic, read_bounded_private_file, regular_file_exists,
        remove_private_file_if_present,
    },
    DiscoverySpoolError, DiscoverySpoolRecordV1, DiscoverySpoolV1, FileLock, PendingDiscoveryV1,
    RetirementIntentV1, RetirementReportV1, StoredDiscoveryAckV1, ACK_RECORD_MAGIC, ACK_SUFFIX,
    LOCK_FILE, OFFER_RECORD_MAGIC, OFFER_SUFFIX, PENDING_RECORD_MAGIC, PENDING_SUFFIX,
    QUARANTINE_MAGIC, QUARANTINE_SUFFIX, RECORD_VERSION, RETIREMENT_FIXED_BYTES, RETIREMENT_MAGIC,
    RETIREMENT_SUFFIX, TEMP_SUFFIX,
};

impl DiscoverySpoolV1 {
    /// Completes every prepared retirement authorized by the durable closure
    /// checkpoint. Records prepared for a later checkpoint remain untouched.
    pub fn complete_retirements_through(
        &self,
        closed_height: u64,
    ) -> Result<RetirementReportV1, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, LOCK_FILE)?;
        let entries = fs::read_dir(&self.retirements).map_err(|source| {
            io_error(
                "list discovery retirement intents",
                &self.retirements,
                source,
            )
        })?;
        let mut report = RetirementReportV1::default();
        for entry in entries {
            let entry = entry.map_err(|source| {
                io_error(
                    "read discovery retirement intent",
                    &self.retirements,
                    source,
                )
            })?;
            if self.complete_single_retirement(&entry.path(), closed_height)? {
                report.completed = report
                    .completed
                    .checked_add(1)
                    .ok_or(DiscoverySpoolError::Overflow)?;
            } else {
                report.waiting_for_checkpoint = report
                    .waiting_for_checkpoint
                    .checked_add(1)
                    .ok_or(DiscoverySpoolError::Overflow)?;
            }
        }
        Ok(report)
    }

    fn complete_single_retirement(
        &self,
        path: &Path,
        closed_height: u64,
    ) -> Result<bool, DiscoverySpoolError> {
        let Some(observation) = parse_record_name(path, RETIREMENT_SUFFIX) else {
            return Err(DiscoverySpoolError::UnexpectedEntry(path.to_path_buf()));
        };
        let intent = self.read_retirement_at(path)?;
        if intent.reference.observation_id != observation {
            return Err(DiscoverySpoolError::CorruptRecord {
                path: path.to_path_buf(),
            });
        }
        if intent.closed_through > closed_height {
            return Ok(false);
        }
        self.require_not_quarantined(observation)?;
        self.verify_and_remove_retired_records(observation, &intent, path)?;
        Ok(true)
    }

    fn verify_and_remove_retired_records(
        &self,
        observation: B256,
        intent: &RetirementIntentV1,
        intent_path: &Path,
    ) -> Result<(), DiscoverySpoolError> {
        let offer_path = self.offer_path(&observation);
        let ack_path = self.ack_path(&observation);
        let pending_path = self.pending_path(&observation);
        let offer = if regular_file_exists(&offer_path)? {
            let offer = self.read_offer_at(&offer_path)?;
            if offer.reference != intent.reference {
                return self.latch_conflict(observation, "retirement completion authority");
            }
            Some(offer)
        } else {
            None
        };

        if regular_file_exists(&ack_path)? {
            let offer = offer
                .as_ref()
                .ok_or(DiscoverySpoolError::MissingOffer(observation))?;
            self.read_ack_for_offer(&ack_path, offer)?;
        }
        if regular_file_exists(&pending_path)? {
            verify_pending_marker_at(&pending_path, observation)?;
            if offer.is_none() {
                return Err(DiscoverySpoolError::MissingOffer(observation));
            }
        }

        remove_private_file_if_present(&self.acks, &ack_path, "remove retired ACK")?;
        remove_private_file_if_present(
            &self.pending,
            &pending_path,
            "remove retired pending marker",
        )?;
        remove_private_file_if_present(&self.offers, &offer_path, "remove retired offer")?;
        remove_private_file_if_present(
            &self.retirements,
            intent_path,
            "remove completed retirement intent",
        )
    }

    #[must_use]
    pub fn offer_path(&self, observation: &B256) -> PathBuf {
        self.offers
            .join(format!("{}{OFFER_SUFFIX}", hex_id(observation)))
    }

    #[must_use]
    pub fn offers_root(&self) -> &Path {
        &self.offers
    }

    pub(super) fn ack_path(&self, observation: &B256) -> PathBuf {
        self.acks
            .join(format!("{}{ACK_SUFFIX}", hex_id(observation)))
    }

    pub(super) fn quarantine_path(&self, observation: &B256) -> PathBuf {
        self.quarantine
            .join(format!("{}{QUARANTINE_SUFFIX}", hex_id(observation)))
    }

    pub(super) fn pending_path(&self, observation: &B256) -> PathBuf {
        self.pending
            .join(format!("{}{PENDING_SUFFIX}", hex_id(observation)))
    }

    pub(super) fn retirement_path(&self, observation: &B256) -> PathBuf {
        self.retirements
            .join(format!("{}{RETIREMENT_SUFFIX}", hex_id(observation)))
    }

    pub(super) fn read_retirement_at(
        &self,
        path: &Path,
    ) -> Result<RetirementIntentV1, DiscoverySpoolError> {
        let intent =
            decode_retirement_intent(&read_bounded_private_file(path, RETIREMENT_FIXED_BYTES)?)?;
        if (intent.reference.chain_id, intent.reference.genesis_hash)
            != (self.chain_id, self.genesis_hash)
            || intent.closed_through == 0
        {
            return Err(DiscoverySpoolError::CorruptRecord {
                path: path.to_path_buf(),
            });
        }
        Ok(intent)
    }

    pub(super) fn pending_unlocked(
        &self,
        observation: &B256,
    ) -> Result<Option<PendingDiscoveryV1>, DiscoverySpoolError> {
        self.require_not_quarantined(*observation)?;
        let retirement_path = self.retirement_path(observation);
        if regular_file_exists(&retirement_path)? {
            let retirement = self.read_retirement_at(&retirement_path)?;
            if retirement.reference.observation_id != *observation {
                return Err(DiscoverySpoolError::CorruptRecord {
                    path: retirement_path,
                });
            }
            return Ok(None);
        }
        let path = self.offer_path(observation);
        if !regular_file_exists(&path)? {
            return Ok(None);
        }
        let offer = self.read_offer_at(&path)?;
        let ack_path = self.ack_path(observation);
        if regular_file_exists(&ack_path)? {
            self.read_ack_for_offer(&ack_path, &offer)?;
            self.remove_pending_marker_unlocked(*observation)?;
            return Ok(None);
        }
        self.ensure_pending_marker_unlocked(*observation)?;
        Ok(Some(offer))
    }

    pub(super) fn ensure_pending_marker_unlocked(
        &self,
        observation: B256,
    ) -> Result<(), DiscoverySpoolError> {
        let path = self.pending_path(&observation);
        if regular_file_exists(&path)? {
            return verify_pending_marker_at(&path, observation);
        }
        persist_immutable_atomic(&self.pending, &path, &encode_pending_marker(observation))
    }

    pub(super) fn remove_pending_marker_unlocked(
        &self,
        observation: B256,
    ) -> Result<(), DiscoverySpoolError> {
        let path = self.pending_path(&observation);
        remove_private_file_if_present(&self.pending, &path, "remove acknowledged pending marker")
    }

    pub(super) fn read_offer_at(
        &self,
        path: &Path,
    ) -> Result<PendingDiscoveryV1, DiscoverySpoolError> {
        let max = OFFER_RECORD_MAGIC
            .len()
            .checked_add(DiscoveryOfferRefV1::FIXED_BYTES)
            .and_then(|value| value.checked_add(8))
            .and_then(|value| value.checked_add(self.limits.max_control_body_bytes))
            .and_then(|value| value.checked_add(32))
            .ok_or(DiscoverySpoolError::Overflow)?;
        let encoded = read_bounded_private_file(path, max)?;
        let (reference, canonical) = decode_offer_record(&encoded)?;
        let spec = FinalizedJobSpecV1::decode_body(canonical, &self.limits)?;
        let expected_fields = (
            canonical,
            self.chain_id,
            self.genesis_hash,
            observation_id(self.chain_id, self.genesis_hash, &spec.summary),
            keccak256(canonical),
        );
        let reencoded = spec.encode_body(&self.limits)?;
        let actual_fields = (
            reencoded.as_slice(),
            reference.chain_id,
            reference.genesis_hash,
            reference.observation_id,
            reference.discovery_record_digest,
        );
        if actual_fields != expected_fields {
            return Err(DiscoverySpoolError::CorruptRecord {
                path: path.to_path_buf(),
            });
        }
        Ok(PendingDiscoveryV1 { reference, spec })
    }

    pub(super) fn read_ack_at(
        &self,
        path: &Path,
    ) -> Result<StoredDiscoveryAckV1, DiscoverySpoolError> {
        let max = ACK_RECORD_MAGIC
            .len()
            .checked_add(DiscoveryAckRefV1::FIXED_BYTES)
            .and_then(|value| {
                value.checked_add(8 + 32 + 8 + self.limits.max_control_body_bytes + 32)
            })
            .ok_or(DiscoverySpoolError::Overflow)?;
        let encoded = read_bounded_private_file(path, max)?;
        let (reference, lease_generation, manifest_hash, canonical) = decode_ack_record(&encoded)?;
        let committed = SnapshotExportCommittedV1::decode_body(canonical, &self.limits)?;
        let reencoded = committed.encode_body(&self.limits)?;
        let actual = (
            reencoded.as_slice(),
            reference.chain_id,
            reference.genesis_hash,
            lease_generation == 0,
            manifest_hash.is_zero(),
        );
        let expected = (canonical, self.chain_id, self.genesis_hash, false, false);
        if actual != expected {
            return Err(DiscoverySpoolError::CorruptRecord {
                path: path.to_path_buf(),
            });
        }
        Ok(StoredDiscoveryAckV1 {
            reference,
            committed,
            lease_generation,
            manifest_hash,
        })
    }

    pub(super) fn read_ack_for_offer(
        &self,
        path: &Path,
        offer: &PendingDiscoveryV1,
    ) -> Result<StoredDiscoveryAckV1, DiscoverySpoolError> {
        let ack = self.read_ack_at(path)?;
        if ack.reference.offer_ref() != offer.reference
            || ack.committed.job_id != offer.spec.summary.job_id
            || offer
                .reference
                .generation
                .checked_add(1)
                .is_none_or(|generation| ack.committed.pin_generation != generation)
        {
            return Err(DiscoverySpoolError::CorruptRecord {
                path: path.to_path_buf(),
            });
        }
        Ok(ack)
    }

    pub(super) fn require_not_quarantined(
        &self,
        observation: B256,
    ) -> Result<(), DiscoverySpoolError> {
        if regular_file_exists(&self.quarantine_path(&observation))? {
            Err(DiscoverySpoolError::Quarantined { observation })
        } else {
            Ok(())
        }
    }

    pub(super) fn latch_conflict<T>(
        &self,
        observation: B256,
        reason: &'static str,
    ) -> Result<T, DiscoverySpoolError> {
        let path = self.quarantine_path(&observation);
        if !regular_file_exists(&path)? {
            let mut encoded = Vec::with_capacity(8 + 2 + 32 + 32);
            encoded.extend_from_slice(&QUARANTINE_MAGIC);
            encoded.extend_from_slice(&RECORD_VERSION.to_be_bytes());
            encoded.extend_from_slice(observation.as_slice());
            encoded.extend_from_slice(keccak256(reason.as_bytes()).as_slice());
            persist_immutable_atomic(&self.quarantine, &path, &encoded)?;
        }
        Err(DiscoverySpoolError::ConflictLatched {
            observation,
            reason,
        })
    }

    pub(super) fn verified_receipt_matches_offer(
        &self,
        spec: &FinalizedJobSpecV1,
        receipt: &VerifiedExportReceipt,
        bundle: &ProtocolBundleV1,
    ) -> Result<bool, ProtocolError> {
        let summary = &spec.summary;
        let intent = JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &self.limits)?;
        let intent_id = intent.intent_id(&self.limits)?;
        let job_id = intent.job_id(
            summary.finalized_block_hash,
            summary.finalized_state_root,
            &self.limits,
        )?;
        let protocol_bundle_hash = bundle.protocol_bundle_hash(&self.limits)?;
        receipt
            .manifest()
            .validate_against_bundle(bundle, &self.limits)?;
        let expected_checkpoint = CheckpointIdentityV1 {
            finalized_block_number: summary.cursor,
            finalized_block_hash: summary.finalized_block_hash,
            finalized_state_root: summary.finalized_state_root,
        };
        let manifest = receipt.manifest();
        let intent_matches = (
            intent_id,
            job_id,
            intent.chain_id,
            intent.genesis_hash,
            intent.fork_id,
            intent.protocol_bundle_hash,
            summary.protocol_bundle_hash,
            receipt.job_id(),
            receipt.checkpoint(),
        ) == (
            summary.intent_id,
            summary.job_id,
            self.chain_id,
            self.genesis_hash,
            bundle.fork_id,
            protocol_bundle_hash,
            protocol_bundle_hash,
            summary.job_id,
            &expected_checkpoint,
        );
        let manifest_matches = (
            manifest.protocol_bundle_hash,
            manifest.job_id,
            manifest.attempt,
            &manifest.checkpoint,
            manifest.wwd,
            manifest.tribute_count,
            manifest.tribute_nominal_total,
            manifest.body_codec_id,
            manifest.opening_codec_registry_hash,
        ) == (
            protocol_bundle_hash,
            summary.job_id,
            intent.attempt,
            &expected_checkpoint,
            intent.wwd,
            intent.authenticated_day_count,
            intent.authenticated_day_nominal,
            bundle.tribute_body_codec_id,
            bundle.opening_codec_registry_hash()?,
        );
        Ok(intent_matches && manifest_matches)
    }
}

pub struct DiscoveryPendingCursorV1 {
    pub(super) spool: DiscoverySpoolV1,
    pub(super) entries: ReadDir,
}

impl Iterator for DiscoveryPendingCursorV1 {
    type Item = Result<PendingDiscoveryV1, DiscoverySpoolError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = match self.entries.next()? {
                Ok(entry) => entry,
                Err(source) => {
                    return Some(Err(io_error(
                        "read pending discovery directory",
                        &self.spool.pending,
                        source,
                    )))
                }
            };
            let path = entry.path();
            let Some(observation) = parse_record_name(&path, PENDING_SUFFIX) else {
                return Some(Err(DiscoverySpoolError::UnexpectedEntry(path)));
            };
            match self.spool.pending_from_index(&observation) {
                Ok(Some(pending)) => return Some(Ok(pending)),
                Ok(None) => {}
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

pub(super) fn parse_category_observation(
    path: &Path,
    suffix: &str,
) -> Result<B256, DiscoverySpoolError> {
    parse_record_name(path, suffix).ok_or_else(|| {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(TEMP_SUFFIX))
        {
            DiscoverySpoolError::AmbiguousTemporary(path.to_path_buf())
        } else {
            DiscoverySpoolError::UnexpectedEntry(path.to_path_buf())
        }
    })
}

pub(super) fn verify_pending_marker_at(
    path: &Path,
    observation: B256,
) -> Result<(), DiscoverySpoolError> {
    let bytes =
        read_bounded_private_file(path, PENDING_RECORD_MAGIC.len() + 2 + B256::len_bytes())?;
    if bytes != encode_pending_marker(observation) {
        return Err(DiscoverySpoolError::CorruptRecord {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

pub(super) fn read_quarantine_record_at(
    path: &Path,
    observation: B256,
) -> Result<DiscoverySpoolRecordV1, DiscoverySpoolError> {
    let expected_len = QUARANTINE_MAGIC.len() + 2 + 32 + 32;
    let bytes = read_bounded_private_file(path, expected_len)?;
    let header = (
        bytes.len(),
        bytes.get(..8),
        read_u16(&bytes, 8).ok(),
        read_b256(&bytes, 10).ok(),
    );
    let expected = (
        expected_len,
        Some(QUARANTINE_MAGIC.as_slice()),
        Some(RECORD_VERSION),
        Some(observation),
    );
    if header != expected {
        return Err(DiscoverySpoolError::CorruptRecord {
            path: path.to_path_buf(),
        });
    }
    Ok(DiscoverySpoolRecordV1::Quarantine {
        observation,
        reason_hash: read_b256(&bytes, 42)?,
    })
}

pub(super) fn encode_retirement_intent(intent: &RetirementIntentV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(RETIREMENT_FIXED_BYTES);
    encoded.extend_from_slice(&RETIREMENT_MAGIC);
    encoded.extend_from_slice(&RECORD_VERSION.to_be_bytes());
    encoded.extend_from_slice(&intent.closed_through.to_be_bytes());
    encoded.extend_from_slice(&intent.reference.encode_fixed());
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    encoded
}

fn decode_retirement_intent(encoded: &[u8]) -> Result<RetirementIntentV1, DiscoverySpoolError> {
    let header = (encoded.len(), encoded.get(..8), read_u16(encoded, 8).ok());
    let expected = (
        RETIREMENT_FIXED_BYTES,
        Some(RETIREMENT_MAGIC.as_slice()),
        Some(RECORD_VERSION),
    );
    if header != expected {
        return Err(DiscoverySpoolError::MalformedRecord {
            path: PathBuf::new(),
        });
    }
    verify_record_checksum(encoded)?;
    Ok(RetirementIntentV1 {
        closed_through: read_u64(encoded, 10)?,
        reference: DiscoveryOfferRefV1::decode_fixed(
            &encoded[18..18 + DiscoveryOfferRefV1::FIXED_BYTES],
        )?,
    })
}

pub(super) fn encode_offer_record(
    reference: &DiscoveryOfferRefV1,
    canonical: &[u8],
) -> Result<Vec<u8>, DiscoverySpoolError> {
    let length = u64::try_from(canonical.len()).map_err(|_| DiscoverySpoolError::Overflow)?;
    let mut encoded =
        Vec::with_capacity(8 + DiscoveryOfferRefV1::FIXED_BYTES + 8 + canonical.len() + 32);
    encoded.extend_from_slice(&OFFER_RECORD_MAGIC);
    encoded.extend_from_slice(&reference.encode_fixed());
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(canonical);
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    Ok(encoded)
}

fn decode_offer_record(
    encoded: &[u8],
) -> Result<(DiscoveryOfferRefV1, &[u8]), DiscoverySpoolError> {
    let header = 8 + DiscoveryOfferRefV1::FIXED_BYTES + 8;
    if encoded.len() < header + 32 || encoded[..8] != OFFER_RECORD_MAGIC {
        return Err(DiscoverySpoolError::MalformedRecord {
            path: PathBuf::new(),
        });
    }
    verify_record_checksum(encoded)?;
    let reference =
        DiscoveryOfferRefV1::decode_fixed(&encoded[8..8 + DiscoveryOfferRefV1::FIXED_BYTES])?;
    let body_len = read_u64(encoded, 8 + DiscoveryOfferRefV1::FIXED_BYTES)?;
    let body_len = usize::try_from(body_len).map_err(|_| DiscoverySpoolError::Overflow)?;
    let body_end = header
        .checked_add(body_len)
        .ok_or(DiscoverySpoolError::Overflow)?;
    if body_end + 32 != encoded.len() {
        return Err(DiscoverySpoolError::MalformedRecord {
            path: PathBuf::new(),
        });
    }
    Ok((reference, &encoded[header..body_end]))
}

pub(super) fn encode_ack_record(
    reference: &DiscoveryAckRefV1,
    lease_generation: u64,
    manifest_hash: B256,
    canonical: &[u8],
) -> Result<Vec<u8>, DiscoverySpoolError> {
    if lease_generation == 0 || manifest_hash.is_zero() {
        return Err(DiscoverySpoolError::MalformedRecord {
            path: PathBuf::new(),
        });
    }
    let length = u64::try_from(canonical.len()).map_err(|_| DiscoverySpoolError::Overflow)?;
    let mut encoded =
        Vec::with_capacity(8 + DiscoveryAckRefV1::FIXED_BYTES + 8 + 32 + 8 + canonical.len() + 32);
    encoded.extend_from_slice(&ACK_RECORD_MAGIC);
    encoded.extend_from_slice(&reference.encode_fixed());
    encoded.extend_from_slice(&lease_generation.to_be_bytes());
    encoded.extend_from_slice(manifest_hash.as_slice());
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(canonical);
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    Ok(encoded)
}

fn decode_ack_record(
    encoded: &[u8],
) -> Result<(DiscoveryAckRefV1, u64, B256, &[u8]), DiscoverySpoolError> {
    let lease_offset = 8 + DiscoveryAckRefV1::FIXED_BYTES;
    let manifest_offset = lease_offset + 8;
    let length_offset = manifest_offset + 32;
    let header = length_offset + 8;
    if encoded.len() < header + 32 || encoded[..8] != ACK_RECORD_MAGIC {
        return Err(DiscoverySpoolError::MalformedRecord {
            path: PathBuf::new(),
        });
    }
    verify_record_checksum(encoded)?;
    let reference =
        DiscoveryAckRefV1::decode_fixed(&encoded[8..8 + DiscoveryAckRefV1::FIXED_BYTES])?;
    let lease_generation = read_u64(encoded, lease_offset)?;
    let manifest_hash = read_b256(encoded, manifest_offset)?;
    let body_len = read_u64(encoded, length_offset)?;
    let body_len = usize::try_from(body_len).map_err(|_| DiscoverySpoolError::Overflow)?;
    let body_end = header
        .checked_add(body_len)
        .ok_or(DiscoverySpoolError::Overflow)?;
    if body_end + 32 != encoded.len() {
        return Err(DiscoverySpoolError::MalformedRecord {
            path: PathBuf::new(),
        });
    }
    Ok((
        reference,
        lease_generation,
        manifest_hash,
        &encoded[header..body_end],
    ))
}

fn verify_record_checksum(encoded: &[u8]) -> Result<(), DiscoverySpoolError> {
    let checksum_start =
        encoded
            .len()
            .checked_sub(32)
            .ok_or_else(|| DiscoverySpoolError::MalformedRecord {
                path: PathBuf::new(),
            })?;
    if keccak256(&encoded[..checksum_start]) != B256::from_slice(&encoded[checksum_start..]) {
        return Err(DiscoverySpoolError::CorruptRecord {
            path: PathBuf::new(),
        });
    }
    Ok(())
}

pub(super) fn parse_record_name(path: &Path, suffix: &str) -> Option<B256> {
    let name = path.file_name()?.to_str()?;
    let encoded = name.strip_suffix(suffix)?;
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let bytes = hex::decode(encoded).ok()?;
    Some(B256::from_slice(&bytes))
}

fn hex_id(value: &B256) -> String {
    hex::encode(value.as_slice())
}

fn encode_pending_marker(observation: B256) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(PENDING_RECORD_MAGIC.len() + 2 + B256::len_bytes());
    encoded.extend_from_slice(&PENDING_RECORD_MAGIC);
    encoded.extend_from_slice(&RECORD_VERSION.to_be_bytes());
    encoded.extend_from_slice(observation.as_slice());
    encoded
}

pub(super) fn read_u16(encoded: &[u8], start: usize) -> Result<u16, DiscoverySpoolError> {
    encoded
        .get(start..start + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_be_bytes)
        .ok_or(DiscoverySpoolError::MalformedCheckpoint)
}

pub(super) fn read_u64(encoded: &[u8], start: usize) -> Result<u64, DiscoverySpoolError> {
    encoded
        .get(start..start + 8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_be_bytes)
        .ok_or(DiscoverySpoolError::MalformedCheckpoint)
}

pub(super) fn read_b256(encoded: &[u8], start: usize) -> Result<B256, DiscoverySpoolError> {
    encoded
        .get(start..start + 32)
        .map(B256::from_slice)
        .ok_or(DiscoverySpoolError::MalformedCheckpoint)
}

pub(super) fn io_error(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> DiscoverySpoolError {
    DiscoverySpoolError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
