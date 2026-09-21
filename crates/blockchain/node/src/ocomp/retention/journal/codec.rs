use crate::ocomp::retention::*;

const JOURNAL_MAGIC: [u8; 8] = *b"OUTBPIN1";

const JOURNAL_VERSION: u16 = 6;

const PIN_RECORD_VERSION: u16 = 6;

const PIN_RECORD_MAX_BYTES: usize = 512;

/// The registry has no OCOMP product count limit. Its only cardinality ceiling
/// is the count width committed by the durable journal wire format.
pub(in crate::ocomp::retention) const JOURNAL_RECORD_COUNT_MAX: usize = u16::MAX as usize;

pub(in crate::ocomp::retention) const JOURNAL_MAX_BYTES: usize =
    (PIN_RECORD_MAX_BYTES + B256::len_bytes() + std::mem::size_of::<u16>())
        * JOURNAL_RECORD_COUNT_MAX
        + 8
        + std::mem::size_of::<u16>()
        + std::mem::size_of::<u64>()
        + B256::len_bytes()
        + std::mem::size_of::<u16>()
        + B256::len_bytes();

pub(in crate::ocomp::retention) fn encode_registry(registry: &JobRegistryV1) -> Vec<u8> {
    encode_registry_with(registry, encode_record)
}

fn encode_registry_with(
    registry: &JobRegistryV1,
    encode: impl Fn(PinRecordV1) -> Vec<u8>,
) -> Vec<u8> {
    let mut encoded =
        Vec::with_capacity(8 + 2 + 8 + 32 + 2 + registry.records.len() * PIN_RECORD_MAX_BYTES + 32);
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    encoded.extend_from_slice(&JOURNAL_VERSION.to_be_bytes());
    encoded.extend_from_slice(&registry.generation.to_be_bytes());
    encoded.extend_from_slice(registry.last_updated.as_slice());
    encoded.extend_from_slice(
        &u16::try_from(registry.records.len())
            .expect("journal registry length fits its u16 wire count")
            .to_be_bytes(),
    );
    for (key, record) in &registry.records {
        let record = encode(*record);
        encoded.extend_from_slice(key.as_slice());
        encoded.extend_from_slice(
            &u16::try_from(record.len())
                .expect("bounded pin record length fits u16")
                .to_be_bytes(),
        );
        encoded.extend_from_slice(&record);
    }
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    encoded
}

pub(in crate::ocomp::retention) fn decode_registry(
    encoded: &[u8],
) -> Result<JobRegistryV1, RetentionError> {
    if encoded.len() < JOURNAL_MAGIC.len() + 2 + 32 {
        return Err(RetentionError::MalformedJournal(
            "truncated registry header",
        ));
    }
    let version = u16::from_be_bytes(
        encoded
            .get(8..10)
            .ok_or(RetentionError::MalformedJournal(
                "truncated registry version",
            ))?
            .try_into()
            .map_err(|_| RetentionError::MalformedJournal("registry version length"))?,
    );
    if version != JOURNAL_VERSION {
        return Err(RetentionError::UnsupportedJournalVersion { actual: version });
    }
    let (body, checksum) = encoded.split_at(encoded.len() - 32);
    if keccak256(body).as_slice() != checksum {
        return Err(RetentionError::MalformedJournal("checksum mismatch"));
    }
    let mut reader = JournalReader::new(body);
    if reader.take::<8>()? != JOURNAL_MAGIC {
        return Err(RetentionError::MalformedJournal("wrong magic"));
    }
    let actual = u16::from_be_bytes(reader.take::<2>()?);
    if actual != JOURNAL_VERSION {
        return Err(RetentionError::UnsupportedJournalVersion { actual });
    }
    let generation = u64::from_be_bytes(reader.take::<8>()?);
    if generation == 0 {
        return Err(RetentionError::MalformedJournal("zero registry generation"));
    }
    let last_updated = B256::new(reader.take::<32>()?);
    let count = usize::from(u16::from_be_bytes(reader.take::<2>()?));
    if count == 0 {
        return Err(RetentionError::MalformedJournal(
            "registry must use an absent file for zero records",
        ));
    }
    let mut records = BTreeMap::new();
    for _ in 0..count {
        let key = B256::new(reader.take::<32>()?);
        let length = usize::from(u16::from_be_bytes(reader.take::<2>()?));
        if length == 0 || length > PIN_RECORD_MAX_BYTES {
            return Err(RetentionError::MalformedJournal(
                "pin record length is outside its bound",
            ));
        }
        let end = reader
            .offset
            .checked_add(length)
            .ok_or(RetentionError::MalformedJournal(
                "pin record offset overflow",
            ))?;
        let bytes = reader
            .encoded
            .get(reader.offset..end)
            .ok_or(RetentionError::MalformedJournal("truncated pin record"))?;
        reader.offset = end;
        let record = decode_record(bytes)?;
        if record_candidate(record).block_hash != key || records.insert(key, record).is_some() {
            return Err(RetentionError::MalformedJournal(
                "duplicate or mismatched registry key",
            ));
        }
    }
    reader.finish()?;
    if !records.contains_key(&last_updated)
        || records.values().map(|record| record.generation).max() != Some(generation)
    {
        return Err(RetentionError::MalformedJournal(
            "registry generation or last-updated key is inconsistent",
        ));
    }
    Ok(JobRegistryV1 {
        generation,
        last_updated,
        records,
    })
}

pub(in crate::ocomp::retention) fn encode_record(record: PinRecordV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(PIN_RECORD_MAX_BYTES);
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    encoded.extend_from_slice(&PIN_RECORD_VERSION.to_be_bytes());
    encoded.extend_from_slice(&record.generation.to_be_bytes());
    match record.state {
        PinStateV1::AwaitingJobFinalization { candidate } => {
            encoded.push(1);
            encode_candidate(&mut encoded, candidate);
        }
        PinStateV1::Finalized {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        } => {
            encoded.push(2);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encode_finalized_window(
                &mut encoded,
                finality_recorded_height,
                open_height,
                deadline_height,
            );
        }
        PinStateV1::Exported {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            export,
        } => {
            encoded.push(3);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encode_finalized_window(
                &mut encoded,
                finality_recorded_height,
                open_height,
                deadline_height,
            );
            encode_export_authority(&mut encoded, export);
        }
        PinStateV1::Terminal {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
            terminal_height,
            release_height,
        } => {
            encoded.push(4);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encode_finalized_window(
                &mut encoded,
                finality_recorded_height,
                open_height,
                deadline_height,
            );
            encoded.extend_from_slice(&source_generation.to_be_bytes());
            match export {
                Some(export) => {
                    encoded.push(1);
                    encode_export_authority(&mut encoded, export);
                }
                None => encoded.push(0),
            }
            encoded.extend_from_slice(&terminal_height.to_be_bytes());
            encoded.extend_from_slice(&release_height.to_be_bytes());
        }
        PinStateV1::GcPending {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
            terminal_height,
            release_height,
        } => {
            encoded.push(6);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encode_finalized_window(
                &mut encoded,
                finality_recorded_height,
                open_height,
                deadline_height,
            );
            encoded.extend_from_slice(&source_generation.to_be_bytes());
            match export {
                Some(export) => {
                    encoded.push(1);
                    encode_export_authority(&mut encoded, export);
                }
                None => encoded.push(0),
            }
            encoded.extend_from_slice(&terminal_height.to_be_bytes());
            encoded.extend_from_slice(&release_height.to_be_bytes());
        }
        PinStateV1::Released {
            candidate,
            job_id,
            source_generation,
            observed_height,
            export,
        } => {
            encoded.push(5);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encoded.extend_from_slice(&source_generation.to_be_bytes());
            match export {
                Some(export) => {
                    encoded.push(1);
                    encode_export_authority(&mut encoded, export);
                }
                None => encoded.push(0),
            }
            encoded.extend_from_slice(&observed_height.to_be_bytes());
        }
    }
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    encoded
}

fn encode_candidate(encoded: &mut Vec<u8>, candidate: CandidatePinV1) {
    encoded.extend_from_slice(&candidate.block_number.to_be_bytes());
    encoded.extend_from_slice(candidate.block_hash.as_slice());
    encoded.extend_from_slice(candidate.state_root.as_slice());
    encoded.extend_from_slice(candidate.intent_id.as_slice());
    encoded.extend_from_slice(&candidate.wwd.to_be_bytes());
    encoded.extend_from_slice(candidate.ce_sealed_root.as_slice());
    encoded.extend_from_slice(candidate.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(candidate.input_lease_id.as_slice());
}

fn encode_finalized_window(
    encoded: &mut Vec<u8>,
    finality_recorded_height: u64,
    open_height: u64,
    deadline_height: u64,
) {
    encoded.extend_from_slice(&finality_recorded_height.to_be_bytes());
    encoded.extend_from_slice(&open_height.to_be_bytes());
    encoded.extend_from_slice(&deadline_height.to_be_bytes());
}

fn encode_export_authority(encoded: &mut Vec<u8>, export: ExportAuthorityV1) {
    encoded.extend_from_slice(&export.source_generation.to_be_bytes());
    encoded.extend_from_slice(&export.lease_generation.to_be_bytes());
    encoded.extend_from_slice(export.manifest_hash.as_slice());
}

fn decode_export_authority(
    reader: &mut JournalReader<'_>,
) -> Result<ExportAuthorityV1, RetentionError> {
    let export = ExportAuthorityV1 {
        source_generation: u64::from_be_bytes(reader.take::<8>()?),
        lease_generation: u64::from_be_bytes(reader.take::<8>()?),
        manifest_hash: B256::new(reader.take::<32>()?),
    };
    if export.source_generation == 0
        || export.lease_generation == 0
        || export.manifest_hash.is_zero()
    {
        return Err(RetentionError::MalformedJournal(
            "incomplete export authority",
        ));
    }
    Ok(export)
}

fn decode_record(encoded: &[u8]) -> Result<PinRecordV1, RetentionError> {
    if encoded.len() < JOURNAL_MAGIC.len() + 2 + 8 + 1 + 32 {
        return Err(RetentionError::MalformedJournal("truncated header"));
    }
    let (body, checksum) = encoded.split_at(encoded.len() - 32);
    if keccak256(body).as_slice() != checksum {
        return Err(RetentionError::MalformedJournal("checksum mismatch"));
    }
    let mut reader = JournalReader::new(body);
    if reader.take::<8>()? != JOURNAL_MAGIC {
        return Err(RetentionError::MalformedJournal("wrong magic"));
    }
    let version = u16::from_be_bytes(reader.take::<2>()?);
    if version != PIN_RECORD_VERSION {
        return Err(RetentionError::UnsupportedJournalVersion { actual: version });
    }
    let generation = u64::from_be_bytes(reader.take::<8>()?);
    if generation == 0 {
        return Err(RetentionError::MalformedJournal("zero generation"));
    }
    let tag = reader.take::<1>()?[0];
    let candidate = decode_candidate(&mut reader)?;
    let state = match tag {
        1 => PinStateV1::AwaitingJobFinalization { candidate },
        2 => {
            let job_id = B256::new(reader.take::<32>()?);
            let (finality_recorded_height, open_height, deadline_height) =
                decode_finalized_window(&mut reader)?;
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            }
        }
        3 => {
            let job_id = B256::new(reader.take::<32>()?);
            let (finality_recorded_height, open_height, deadline_height) =
                decode_finalized_window(&mut reader)?;
            let export = decode_export_authority(&mut reader)?;
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
            }
        }
        4 => {
            let job_id = B256::new(reader.take::<32>()?);
            let (finality_recorded_height, open_height, deadline_height) =
                decode_finalized_window(&mut reader)?;
            let source_generation = u64::from_be_bytes(reader.take::<8>()?);
            if source_generation == 0 {
                return Err(RetentionError::MalformedJournal(
                    "zero terminal source generation",
                ));
            }
            let export = match reader.take::<1>()?[0] {
                0 => None,
                1 => Some(decode_export_authority(&mut reader)?),
                _ => {
                    return Err(RetentionError::MalformedJournal(
                        "invalid terminal export-authority flag",
                    ));
                }
            };
            if export.is_some_and(|authority| authority.source_generation != source_generation) {
                return Err(RetentionError::MalformedJournal(
                    "terminal export authority has a conflicting source generation",
                ));
            }
            let terminal_height = u64::from_be_bytes(reader.take::<8>()?);
            let release_height = u64::from_be_bytes(reader.take::<8>()?);
            if terminal_height.checked_add(RETAINED_EVIDENCE_WINDOW_BLOCKS) != Some(release_height)
            {
                return Err(RetentionError::MalformedJournal(
                    "release height is not terminal finality plus evidence window",
                ));
            }
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                export,
                terminal_height,
                release_height,
            }
        }
        5 => {
            let job_id = B256::new(reader.take::<32>()?);
            let source_generation = u64::from_be_bytes(reader.take::<8>()?);
            if source_generation == 0 {
                return Err(RetentionError::MalformedJournal(
                    "zero released source generation",
                ));
            }
            let export = match reader.take::<1>()?[0] {
                0 => None,
                1 => Some(decode_export_authority(&mut reader)?),
                _ => {
                    return Err(RetentionError::MalformedJournal(
                        "invalid export-authority flag",
                    ));
                }
            };
            let valid_authority =
                export.is_none_or(|authority| authority.source_generation == source_generation);
            if !valid_authority {
                return Err(RetentionError::MalformedJournal(
                    "released record carries inconsistent authority",
                ));
            }
            PinStateV1::Released {
                candidate,
                job_id,
                source_generation,
                observed_height: u64::from_be_bytes(reader.take::<8>()?),
                export,
            }
        }
        6 => {
            let job_id = B256::new(reader.take::<32>()?);
            let (finality_recorded_height, open_height, deadline_height) =
                decode_finalized_window(&mut reader)?;
            let source_generation = u64::from_be_bytes(reader.take::<8>()?);
            if source_generation == 0 {
                return Err(RetentionError::MalformedJournal(
                    "zero GC source generation",
                ));
            }
            let export = match reader.take::<1>()?[0] {
                0 => None,
                1 => Some(decode_export_authority(&mut reader)?),
                _ => {
                    return Err(RetentionError::MalformedJournal(
                        "invalid GC export-authority flag",
                    ));
                }
            };
            if export.is_some_and(|authority| authority.source_generation != source_generation) {
                return Err(RetentionError::MalformedJournal(
                    "GC export authority has a conflicting source generation",
                ));
            }
            let terminal_height = u64::from_be_bytes(reader.take::<8>()?);
            let release_height = u64::from_be_bytes(reader.take::<8>()?);
            if terminal_height.checked_add(RETAINED_EVIDENCE_WINDOW_BLOCKS) != Some(release_height)
            {
                return Err(RetentionError::MalformedJournal(
                    "GC release height is not terminal finality plus evidence window",
                ));
            }
            PinStateV1::GcPending {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                export,
                terminal_height,
                release_height,
            }
        }
        _ => return Err(RetentionError::MalformedJournal("unknown state tag")),
    };
    reader.finish()?;
    Ok(PinRecordV1 { generation, state })
}

fn decode_candidate(reader: &mut JournalReader<'_>) -> Result<CandidatePinV1, RetentionError> {
    Ok(CandidatePinV1 {
        block_number: u64::from_be_bytes(reader.take::<8>()?),
        block_hash: B256::new(reader.take::<32>()?),
        state_root: B256::new(reader.take::<32>()?),
        intent_id: B256::new(reader.take::<32>()?),
        wwd: u32::from_be_bytes(reader.take::<4>()?),
        ce_sealed_root: B256::new(reader.take::<32>()?),
        protocol_bundle_hash: B256::new(reader.take::<32>()?),
        input_lease_id: B256::new(reader.take::<32>()?),
    })
}

fn decode_finalized_window(
    reader: &mut JournalReader<'_>,
) -> Result<(u64, u64, u64), RetentionError> {
    let finality_recorded_height = u64::from_be_bytes(reader.take::<8>()?);
    let open_height = u64::from_be_bytes(reader.take::<8>()?);
    let deadline_height = u64::from_be_bytes(reader.take::<8>()?);
    if finality_recorded_height
        .checked_add(outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH)
        != Some(open_height)
        || open_height >= deadline_height
    {
        return Err(RetentionError::MalformedJournal(
            "invalid finalized response window",
        ));
    }
    Ok((finality_recorded_height, open_height, deadline_height))
}

struct JournalReader<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> JournalReader<'a> {
    const fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], RetentionError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(RetentionError::MalformedJournal("offset overflow"))?;
        let value = self
            .encoded
            .get(self.offset..end)
            .ok_or(RetentionError::MalformedJournal("truncated field"))?;
        self.offset = end;
        value
            .try_into()
            .map_err(|_| RetentionError::MalformedJournal("field length"))
    }

    fn finish(self) -> Result<(), RetentionError> {
        if self.offset != self.encoded.len() {
            return Err(RetentionError::MalformedJournal("trailing bytes"));
        }
        Ok(())
    }
}
