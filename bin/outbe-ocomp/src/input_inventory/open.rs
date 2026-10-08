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
        return Err(TributeInventoryError::Corrupt("owner inventory digest"));
    }
    verify_owner_file(
        &root.join(OWNERS_FILE),
        header.unique_owner_count,
        &on_progress,
    )?;
    if digest_file_observing(&root.join(BODIES_FILE), &on_progress)? != header.body_file_digest {
        return Err(TributeInventoryError::Corrupt("Tribute body spool digest"));
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
        return Err(TributeInventoryError::Corrupt("reference ISO inventory"));
    }
    Ok(SealedTributeInventory {
        root,
        metadata: InventoryMetadata { header, isos },
        _lock: lock,
    })
}
