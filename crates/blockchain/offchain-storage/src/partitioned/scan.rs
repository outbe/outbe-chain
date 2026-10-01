//! Per-call ordered pagination over injected readers. Prefetch never becomes a logical cursor.

use crate::{
    Key, Namespace, ScanEntry, ScanPage, ScanRequest, StorageError, StorageReaderHandle,
    MAX_SCAN_PAGE_VALUE_BYTES,
};
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
};

/// Internal tuning; independent of datasource selection and entity routing.
struct ScanPolicy {
    read_ahead_entries: usize,
    read_ahead_bytes: usize,
}
const POLICY: ScanPolicy = ScanPolicy {
    read_ahead_entries: 64,
    read_ahead_bytes: 8 * 1024 * 1024,
};

struct PartitionCursor {
    reader: StorageReaderHandle,
    entries: VecDeque<ScanEntry>,
    after: Option<Key>,
    exhausted: bool,
}

pub(super) fn merge(
    readers: Vec<StorageReaderHandle>,
    namespace: Namespace,
    request: ScanRequest<'_>,
) -> Result<ScanPage, StorageError> {
    let mut cursors = Vec::with_capacity(readers.len());
    let mut heads = BinaryHeap::new();
    let mut read_ahead_bytes = 0;
    for reader in readers {
        let mut cursor = PartitionCursor {
            reader,
            entries: VecDeque::new(),
            after: request.after().cloned(),
            exhausted: false,
        };
        cursor.refill(&namespace, request.prefix(), 1, &mut read_ahead_bytes)?;
        if let Some(entry) = cursor.entries.front() {
            heads.push(Reverse((entry.key.clone(), cursors.len())));
        }
        cursors.push(cursor);
    }
    let mut result = ScanPage::default();
    let mut output_bytes = 0;
    while let Some(Reverse((key, index))) = heads.pop() {
        if heads
            .peek()
            .is_some_and(|Reverse((other, _))| *other == key)
        {
            return Err(corruption("duplicate entity key across partitions"));
        }
        let cursor = &mut cursors[index];
        let entry = cursor
            .entries
            .pop_front()
            .expect("heap head belongs to cursor");
        if let Some(head) = cursor.entries.front() {
            // Promoting a buffered record to the necessary head frees prefetch budget.
            read_ahead_bytes -= payload_bytes(head);
        }
        let size = payload_bytes(&entry);
        if result.entries.len() == request.limit()
            || output_bytes + size > MAX_SCAN_PAGE_VALUE_BYTES
        {
            result.next_after = Some(
                result
                    .entries
                    .last()
                    .ok_or_else(|| corruption("partition record exceeds page bound"))?
                    .key
                    .clone(),
            );
            return Ok(result);
        }
        output_bytes += size;
        result.entries.push(entry);
        if cursor.entries.is_empty() && !cursor.exhausted {
            cursor.refill(
                &namespace,
                request.prefix(),
                POLICY.read_ahead_entries,
                &mut read_ahead_bytes,
            )?;
        }
        if let Some(head) = cursor.entries.front() {
            heads.push(Reverse((head.key.clone(), index)));
        }
    }
    Ok(result)
}

impl PartitionCursor {
    fn refill(
        &mut self,
        namespace: &Namespace,
        prefix: &[u8],
        limit: usize,
        read_ahead_bytes: &mut usize,
    ) -> Result<(), StorageError> {
        let request = ScanRequest::new(prefix, self.after.as_ref(), limit)?;
        // The adapter may return up to one 8 MiB page temporarily, even for limit=1.
        let page = self.reader.scan_prefix(namespace.clone(), request)?;
        validate_page(&page, request)?;
        self.exhausted = page.next_after.is_none();
        for entry in page.entries {
            if !self.entries.is_empty() {
                let bytes = payload_bytes(&entry);
                if *read_ahead_bytes + bytes > POLICY.read_ahead_bytes {
                    // Discard the unread tail. Resume after the last retained key, never
                    // after the adapter's cursor, so the next refill cannot skip records.
                    self.exhausted = false;
                    break;
                }
                *read_ahead_bytes += bytes;
            }
            self.after = Some(entry.key.clone());
            self.entries.push_back(entry);
        }
        Ok(())
    }
}

pub(super) fn validate_page(page: &ScanPage, request: ScanRequest<'_>) -> Result<(), StorageError> {
    if page.entries.len() > request.limit() {
        return Err(corruption("partition scan exceeds entry bound"));
    }
    let mut previous = request.after();
    let mut bytes = 0;
    for entry in &page.entries {
        if !entry.key.as_bytes().starts_with(request.prefix())
            || previous.is_some_and(|key| key >= &entry.key)
        {
            return Err(corruption("partition scan violates prefix or ordering"));
        }
        bytes += payload_bytes(entry);
        if bytes > MAX_SCAN_PAGE_VALUE_BYTES {
            return Err(corruption("partition scan exceeds page byte bound"));
        }
        previous = Some(&entry.key);
    }
    if page
        .next_after
        .as_ref()
        .is_some_and(|cursor| page.entries.last().is_none_or(|entry| &entry.key != cursor))
    {
        return Err(corruption(
            "partition scan continuation is not its last returned key",
        ));
    }
    Ok(())
}

fn payload_bytes(entry: &ScanEntry) -> usize {
    entry.value.as_bytes().len()
        + entry
            .metadata
            .as_ref()
            .map_or(0, crate::StorageMetadata::encoded_len)
}
fn corruption(message: &str) -> StorageError {
    StorageError::Corruption(message.into())
}
