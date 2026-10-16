//! Open an inventory after all authority and file checks pass.

use super::*;

pub fn open_sealed_inventory(
    root: impl AsRef<Path>,
    expected_subject: TributeInventorySubjectV1,
) -> Result<SealedTributeInventory, TributeInventoryError> {
    open_sealed_inventory_observing(root, expected_subject, || {})
}

pub fn open_sealed_inventory_observing(
    root: impl AsRef<Path>,
    expected_subject: TributeInventorySubjectV1,
    on_progress: impl Fn(),
) -> Result<SealedTributeInventory, TributeInventoryError> {
    let root = root.as_ref().to_path_buf();
    inspect_private_directory(&root)?;
    let lock = InventoryLock::acquire(&root)?;
    let header = decode_header(&read_exact_file(
        &root.join(HEADER_FILE),
        super::header::header_len(),
    )?)?;
    if header.subject != expected_subject {
        return Err(TributeInventoryError::Authority("inventory subject"));
    }
    let owner_digest = digest_file_observing(&root.join(OWNERS_FILE), &on_progress)?;
    if owner_digest != header.owner_file_digest {
        return Err(TributeInventoryError::CorruptOwnerInventoryDigest);
    }
    verify_owner_file(
        &root.join(OWNERS_FILE),
        header.unique_owner_count,
        &on_progress,
    )?;
    if digest_file_observing(&root.join(BODIES_FILE), &on_progress)? != header.body_file_digest {
        return Err(TributeInventoryError::CorruptTributeBodySpoolDigest);
    }
    verify_body_spool(
        &root.join(BODIES_FILE),
        expected_subject.expected_tribute_count,
        header.exact_body_bytes,
        &on_progress,
    )?;
    let iso_bytes = read_exact_file(&root.join(ISOS_FILE), ISO_BITMAP_BYTES)?;
    let mut isos = Box::new([0_u8; ISO_BITMAP_BYTES]);
    isos.copy_from_slice(&iso_bytes);
    if B256::from_slice(&Keccak256::digest(&isos[..])) != header.iso_bitmap_digest
        || !contains_iso(&isos, 840)
    {
        return Err(TributeInventoryError::CorruptReferenceIsoInventory);
    }
    let sources = StateTributeProofArchive::open(
        root.join(SOURCE_PROOF_ARCHIVE_DIRECTORY),
        expected_subject.state_source(),
        &on_progress,
    )?;
    let mut bodies = TributeBodySpoolReader::open(
        root.join(BODIES_FILE),
        expected_subject.expected_tribute_count,
        header.exact_body_bytes,
    )?;
    let mut owner_file = open_regular_readonly(&root.join(OWNERS_FILE))?;
    // One canonical owner/day identity exists per owner, so exact body coverage
    // plus exact owner count and membership establish complete owner coverage.
    if header.unique_owner_count != u64::from(expected_subject.expected_tribute_count) {
        return Err(TributeInventoryError::Authority("state owner coverage"));
    }
    let mut previous = None;
    let mut expected_isos = Box::new([0_u8; ISO_BITMAP_BYTES]);
    set_iso(&mut expected_isos, 840);
    while let Some(bytes) =
        bodies.next_body(outbe_ocomp_protocol::profile::poc_schema_limits().max_bounded_bytes)?
    {
        let record = outbe_tribute::record::decode_canonical(&bytes)
            .map_err(|_| TributeInventoryError::Authority("canonical Tribute body"))?;
        if previous.is_some_and(|id| id >= record.tribute_id)
            || record.encrypted() != Some(&sources.record(record.tribute_id)?.tribute)
            || !contains_owner(&mut owner_file, header.unique_owner_count, record.owner)?
        {
            return Err(TributeInventoryError::Authority(
                "state inventory source coverage",
            ));
        }
        previous = Some(record.tribute_id);
        set_iso(&mut expected_isos, record.reference_currency);
        on_progress();
    }
    if expected_isos != isos {
        return Err(TributeInventoryError::Authority(
            "state reference currency coverage",
        ));
    }
    Ok(SealedTributeInventory {
        root,
        metadata: InventoryMetadata { header, isos },
        _lock: lock,
    })
}

fn contains_owner(
    file: &mut File,
    count: u64,
    owner: Address,
) -> Result<bool, TributeInventoryError> {
    let mut lower = 0;
    let mut upper = count;
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        file.seek(SeekFrom::Start(
            RUN_HEADER_BYTES + middle * OWNER_BYTES as u64,
        ))
        .map_err(|error| io_error("seek owner membership", Path::new(OWNERS_FILE), error))?;
        match read_owner(file)?.cmp(&owner) {
            Ordering::Less => lower = middle + 1,
            Ordering::Greater => upper = middle,
            Ordering::Equal => return Ok(true),
        }
    }
    Ok(false)
}
