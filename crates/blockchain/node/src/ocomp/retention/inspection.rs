use crate::ocomp::retention::*;

/// Decode one exact production journal through the same bounded codec used at
/// node startup. `root` is the node's `ocomp_retention` directory.
pub fn inspect_retention_journal(
    root: impl AsRef<Path>,
) -> Result<RetentionJournalSnapshotV1, RetentionError> {
    let path = root.as_ref().join(JOURNAL_FILENAME);
    let metadata = fs::symlink_metadata(&path).map_err(|source| RetentionError::Io {
        operation: "stat",
        path: path.clone(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(RetentionError::AmbiguousJournal(
            "journal is not a regular file",
        ));
    }
    if metadata.len() > JOURNAL_MAX_BYTES as u64 {
        return Err(RetentionError::MalformedJournal("journal exceeds byte cap"));
    }
    let mut file = File::open(&path).map_err(|source| RetentionError::Io {
        operation: "open",
        path: path.clone(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|source| RetentionError::Io {
            operation: "read",
            path: path.clone(),
            source,
        })?;
    let registry = decode_registry(&bytes)?;
    Ok(RetentionJournalSnapshotV1 {
        generation: registry.generation,
        last_updated: registry.last_updated,
        records: registry.records.into_iter().collect(),
    })
}
