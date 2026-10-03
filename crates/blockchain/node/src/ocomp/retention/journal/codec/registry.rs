use super::*;

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
    append_checksum(encoded)
}

pub(in crate::ocomp::retention) fn decode_registry(
    encoded: &[u8],
) -> Result<JobRegistryV1, RetentionError> {
    check_registry_version(encoded)?;
    let body = checked_body(
        encoded,
        JOURNAL_MAGIC.len() + 2 + 32,
        "truncated registry header",
    )?;
    let mut reader = JournalReader::new(body);
    read_version(&mut reader, JOURNAL_VERSION)?;
    let generation = read_generation(&mut reader, "zero registry generation")?;
    let last_updated = B256::new(reader.take::<32>()?);
    let count = usize::from(u16::from_be_bytes(reader.take::<2>()?));
    if count == 0 {
        return Err(RetentionError::MalformedJournal(
            "registry must use an absent file for zero records",
        ));
    }
    let records = read_records(&mut reader, count)?;
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

fn check_registry_version(encoded: &[u8]) -> Result<(), RetentionError> {
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
    Ok(())
}

fn read_records(
    reader: &mut JournalReader<'_>,
    count: usize,
) -> Result<BTreeMap<B256, PinRecordV1>, RetentionError> {
    let mut records = BTreeMap::new();
    for _ in 0..count {
        let key = B256::new(reader.take::<32>()?);
        let record = decode_record(reader.record_bytes()?)?;
        if record_candidate(record).block_hash != key || records.insert(key, record).is_some() {
            return Err(RetentionError::MalformedJournal(
                "duplicate or mismatched registry key",
            ));
        }
    }
    Ok(records)
}
