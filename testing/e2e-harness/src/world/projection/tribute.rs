use super::*;

pub(super) fn projected_from_readers(
    readers: &[StorageReaderHandle],
    tx_hash: &str,
) -> Result<ProjectedTribute> {
    let record = find_primary(readers, tx_hash)?.1;
    Ok(ProjectedTribute {
        raw_id: WwdEntityId::try_from(record.key.as_bytes())?,
        stored_body: record.value.as_bytes().to_vec(),
    })
}

pub(super) fn tribute_readers(cfg: &Config, index: usize) -> Result<Vec<StorageReaderHandle>> {
    Ok(vec![session(cfg, index)?])
}

pub(super) fn open_tribute_readers(
    offchain_root: &Path,
    secondary_root: &Path,
) -> Result<Vec<StorageReaderHandle>> {
    let source = outbe_offchain_storage::partitioned::adapters::RocksPartitionReadView::open(
        offchain_root,
        secondary_root,
    )?;
    Ok(vec![Arc::new(
        outbe_offchain_storage::PartitionedStorage::read_only(
            Arc::new(source),
            outbe_offchain_data::entity_partition_routing()?,
        ),
    )])
}

pub(super) fn find_primary(
    readers: &[StorageReaderHandle],
    tx_hash: &str,
) -> Result<(usize, ScanEntry)> {
    let mut found = None;
    for (index, reader) in readers.iter().enumerate() {
        match primary(reader.as_ref(), tx_hash) {
            Ok(entry) => {
                if found.is_some() {
                    bail!("multiple Tribute records for transaction {tx_hash}");
                }
                found = Some((index, entry));
            }
            Err(error) if error.to_string().contains("no Tribute") => {}
            Err(error) => return Err(error),
        }
    }
    found.ok_or_else(|| eyre!("no Tribute for transaction {tx_hash}"))
}

pub(super) fn snapshot_across(
    readers: &[StorageReaderHandle],
    tx_hash: &str,
) -> Result<TributeProjectionSnapshot> {
    let (index, _) = find_primary(readers, tx_hash)?;
    snapshot(readers[index].as_ref(), tx_hash)
}

pub(super) fn primary(reader: &dyn StorageReader, tx_hash: &str) -> Result<ScanEntry> {
    let namespace = Namespace::new(COLLECTIONS[0])?;
    let mut after = None;
    let mut found = None;
    loop {
        let page = reader.scan_prefix(
            namespace.clone(),
            ScanRequest::new(&[], after.as_ref(), 256)?,
        )?;
        for entry in page.entries {
            if entry
                .metadata
                .as_ref()
                .and_then(|m| m.get("tx_hash"))
                .is_some_and(|tx| tx.eq_ignore_ascii_case(tx_hash))
                && found.replace(entry).is_some()
            {
                bail!("multiple Tribute records for transaction {tx_hash}");
            }
        }
        after = page.next_after;
        if after.is_none() {
            break;
        }
    }
    found.ok_or_else(|| eyre!("no Tribute for transaction {tx_hash}"))
}

pub(super) fn snapshot(
    reader: &dyn StorageReader,
    tx_hash: &str,
) -> Result<TributeProjectionSnapshot> {
    let primary = primary(reader, tx_hash)?;
    let raw_id = WwdEntityId::try_from(primary.key.as_bytes())?;
    let (tribute_id, owner, day) = match decode_stored_tribute_v2(primary.value.as_bytes()) {
        Ok(body) => (
            body.context.tribute_id,
            body.context.owner,
            body.context.worldwide_day,
        ),
        Err(_) => {
            let body = decode_stored_tribute_v1(primary.value.as_bytes())
                .wrap_err("decode projected Tribute")?;
            (body.tribute_id, body.owner, body.worldwide_day)
        }
    };
    if tribute_id != raw_id {
        bail!("Tribute primary key does not match its body");
    }
    let owner_key = [owner.as_slice(), raw_id.as_slice()].concat();
    let day_key = [day.value().to_be_bytes().as_slice(), raw_id.as_slice()].concat();
    let index = |name: &str, key: Vec<u8>| -> Result<ScanEntry> {
        let key = Key::new(key)?;
        let record = reader
            .get_record(Namespace::new(name)?, &key)?
            .ok_or_else(|| eyre!("missing {name} index"))?;
        if !record.value.as_bytes().is_empty() {
            bail!("{name} index value must be empty");
        }
        Ok(ScanEntry {
            key,
            value: record.value,
            metadata: record.metadata,
        })
    };
    Ok(TributeProjectionSnapshot {
        records: [
            primary,
            index(COLLECTIONS[1], owner_key)?,
            index(COLLECTIONS[2], day_key)?,
        ],
    })
}
