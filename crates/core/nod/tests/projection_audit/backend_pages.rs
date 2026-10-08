use super::*;

#[derive(Clone, Copy)]
enum PageMode {
    Short,
    Descending,
    Duplicate,
    WrongContinuation,
    EmptyContinuation,
    Oversized,
    RepeatCursor,
    LateError,
}

struct PageReader {
    storage: Arc<MemoryStorage>,
    mode: PageMode,
}

impl StorageReader for PageReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.storage.get_record(namespace, key)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        if matches!(self.mode, PageMode::LateError) && request.after().is_some() {
            return Err(StorageError::Corruption("interrupted later page".into()));
        }
        let limit = if matches!(self.mode, PageMode::Short | PageMode::LateError) {
            1
        } else {
            request.limit()
        };
        let mut page = self.storage.scan_prefix(
            namespace.clone(),
            ScanRequest::new(request.prefix(), request.after(), limit)?,
        )?;
        self.mutate_page(namespace, request, &mut page)?;
        Ok(page)
    }
}

impl PageReader {
    fn mutate_page(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
        page: &mut ScanPage,
    ) -> Result<(), StorageError> {
        match self.mode {
            PageMode::Short | PageMode::LateError => {}
            PageMode::Descending => page.entries.reverse(),
            PageMode::Duplicate => {
                if let Some(first) = page.entries.first().cloned() {
                    page.entries.insert(0, first);
                }
            }
            PageMode::WrongContinuation => page.next_after = Some(Key::new(vec![0xff])?),
            PageMode::EmptyContinuation => {
                page.entries.clear();
                page.next_after = Some(Key::new(vec![0xff])?);
            }
            PageMode::Oversized => {
                if let Some(first) = page.entries.first().cloned() {
                    page.entries.resize(request.limit() + 1, first);
                }
                page.next_after = None;
            }
            PageMode::RepeatCursor => {
                if let Some(after) = request.after() {
                    if let Some(record) = self.storage.get_record(namespace, after)? {
                        page.entries.insert(
                            0,
                            ScanEntry {
                                key: after.clone(),
                                value: record.value,
                                metadata: record.metadata,
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[test]
fn primary_scans_reject_invalid_backend_page_order_size_and_continuations() {
    for mode in [
        PageMode::Descending,
        PageMode::Duplicate,
        PageMode::WrongContinuation,
        PageMode::EmptyContinuation,
        PageMode::Oversized,
        PageMode::RepeatCursor,
    ] {
        let storage = fixture();
        let before = snapshot(&storage);
        let reader = outbe_nod::nod_reader(Arc::new(PageReader {
            storage: storage.clone(),
            mode,
        }));
        for bucket in [false, true] {
            assert!(scan(
                &reader,
                bucket,
                IdPageRequest {
                    after: Some(id(if bucket { 11 } else { 1 })),
                    limit: 2
                }
            )
            .is_err());
        }
        assert_eq!(snapshot(&storage), before);
    }
}

#[test]
fn audits_follow_short_pages_and_propagate_later_page_failures() {
    let storage = fixture();
    let before = snapshot(&storage);
    let reader = outbe_nod::nod_reader(Arc::new(PageReader {
        storage: storage.clone(),
        mode: PageMode::Short,
    }));
    for bucket in [false, true] {
        let mut after = None;
        let mut ids = Vec::new();
        loop {
            let page = scan(&reader, bucket, IdPageRequest { after, limit: 10 }).unwrap();
            assert_eq!(page.entries.len(), 1);
            ids.push(page.entries[0].0);
            after = page.next_after;
            if after.is_none() {
                break;
            }
        }
        assert_eq!(
            ids,
            if bucket {
                vec![id(11), id(12), id(13)]
            } else {
                vec![id(1), id(2), id(3)]
            }
        );
    }
    reader.audit_indexes(&work()).unwrap();
    let interrupted = outbe_nod::nod_reader(Arc::new(PageReader {
        storage: storage.clone(),
        mode: PageMode::LateError,
    }));
    assert!(interrupted
        .audit_indexes(&work())
        .unwrap_err()
        .to_string()
        .contains("interrupted later page"));
    assert_eq!(snapshot(&storage), before);
}
