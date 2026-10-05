//! Ordered logical scans over one captured pending view and lazy durable pages.

use std::{cmp::Ordering, collections::VecDeque};

use super::{PendingCore, PendingRecord};
use crate::{
    Key, Namespace, ScanEntry, ScanPage, ScanRequest, StorageError, MAX_SCAN_ENTRIES,
    MAX_SCAN_PAGE_VALUE_BYTES,
};

impl PendingCore {
    pub(super) fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        request.validate()?;
        let prefix = request.prefix().to_vec();
        let after = request.after().cloned();
        let pending = self.scan_pending_records(&namespace, &prefix, after.as_ref());
        let base = BaseScanCursor {
            core: self,
            namespace: &namespace,
            prefix: &prefix,
            after,
            entries: VecDeque::new(),
            exhausted: false,
        };
        let mut merged = MergedScan {
            base,
            pending,
            pending_index: 0,
        };
        let mut page = ScanPageBudget::new(request.limit());
        while let Some(entry) = merged.next_entry()? {
            if !page.push(entry) {
                break;
            }
        }
        page.finish()
    }

    fn scan_pending_records(
        &self,
        namespace: &Namespace,
        prefix: &[u8],
        after: Option<&Key>,
    ) -> Vec<(Key, PendingRecord)> {
        self.state
            .read()
            .records
            .get(&namespace.logical())
            .map(|records| {
                records
                    .iter()
                    .filter(|(key, _)| {
                        key.as_bytes().starts_with(prefix) && after.is_none_or(|after| *key > after)
                    })
                    .map(|(key, record)| (key.clone(), record.record.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

struct BaseScanCursor<'a> {
    core: &'a PendingCore,
    namespace: &'a Namespace,
    prefix: &'a [u8],
    after: Option<Key>,
    entries: VecDeque<ScanEntry>,
    exhausted: bool,
}

impl BaseScanCursor<'_> {
    fn refill(&mut self) -> Result<(), StorageError> {
        if !self.entries.is_empty() || self.exhausted {
            return Ok(());
        }
        let page = self.core.base.scan_prefix(
            self.namespace.clone(),
            ScanRequest::new(self.prefix, self.after.as_ref(), MAX_SCAN_ENTRIES)?,
        )?;
        if page.entries.is_empty() {
            if page.next_after.is_some() {
                return Err(StorageError::Corruption(
                    "base scan returned an empty page with a continuation".to_owned(),
                ));
            }
            self.exhausted = true;
        } else {
            self.exhausted = page.next_after.is_none();
            if let Some(after) = page.next_after {
                self.after = Some(after);
            }
            self.entries.extend(page.entries);
        }
        Ok(())
    }
}

enum ScanCandidate {
    Entry(ScanEntry),
    Deleted,
    End,
}

struct MergedScan<'a> {
    base: BaseScanCursor<'a>,
    pending: Vec<(Key, PendingRecord)>,
    pending_index: usize,
}

impl MergedScan<'_> {
    fn next_entry(&mut self) -> Result<Option<ScanEntry>, StorageError> {
        loop {
            self.base.refill()?;
            let candidate = match self.next_candidate() {
                ScanCandidate::End => return Ok(None),
                ScanCandidate::Deleted => continue,
                ScanCandidate::Entry(entry) => entry,
            };
            if !self
                .base
                .core
                .is_retired(self.base.namespace, &candidate.key)?
            {
                return Ok(Some(candidate));
            }
        }
    }

    fn next_candidate(&mut self) -> ScanCandidate {
        let base_key = self.base.entries.front().map(|entry| &entry.key);
        let pending_key = self.pending.get(self.pending_index).map(|(key, _)| key);
        let ordering = match (base_key, pending_key) {
            (None, None) => return ScanCandidate::End,
            (Some(_), None) => return self.take_base(),
            (None, Some(_)) => return self.take_pending(),
            (Some(base_key), Some(pending_key)) => base_key.cmp(pending_key),
        };
        match ordering {
            Ordering::Less => self.take_base(),
            Ordering::Equal => {
                self.base.entries.pop_front();
                self.take_pending()
            }
            Ordering::Greater => self.take_pending(),
        }
    }

    fn take_base(&mut self) -> ScanCandidate {
        self.base
            .entries
            .pop_front()
            .map_or(ScanCandidate::End, ScanCandidate::Entry)
    }

    fn take_pending(&mut self) -> ScanCandidate {
        let (key, record) = &self.pending[self.pending_index];
        self.pending_index += 1;
        match record {
            PendingRecord::Put(record) => ScanCandidate::Entry(ScanEntry {
                key: key.clone(),
                value: record.value.clone(),
                metadata: record.metadata.clone(),
            }),
            PendingRecord::Delete => ScanCandidate::Deleted,
        }
    }
}

struct ScanPageBudget {
    entries: Vec<ScanEntry>,
    value_bytes: usize,
    limit: usize,
    has_more: bool,
}

impl ScanPageBudget {
    fn new(limit: usize) -> Self {
        Self {
            entries: Vec::new(),
            value_bytes: 0,
            limit,
            has_more: false,
        }
    }

    fn push(&mut self, entry: ScanEntry) -> bool {
        let entry_bytes = entry.value.as_bytes().len()
            + entry
                .metadata
                .as_ref()
                .map_or(0, |metadata| metadata.encoded_len());
        if self.entries.len() == self.limit
            || self.value_bytes.saturating_add(entry_bytes) > MAX_SCAN_PAGE_VALUE_BYTES
        {
            self.has_more = true;
            return false;
        }
        self.value_bytes += entry_bytes;
        self.entries.push(entry);
        true
    }

    fn finish(self) -> Result<ScanPage, StorageError> {
        let next_after = if self.has_more {
            Some(
                self.entries
                    .last()
                    .ok_or_else(|| {
                        StorageError::Corruption(
                            "stored record exceeds the scan page byte bound".to_owned(),
                        )
                    })?
                    .key
                    .clone(),
            )
        } else {
            None
        };
        Ok(ScanPage {
            entries: self.entries,
            next_after,
        })
    }
}
