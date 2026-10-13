//! Bounded repository scans with checked ordering and continuation keys.
use crate::{
    Key, Namespace, ScanEntry, ScanPage, ScanRequest, StorageError, StorageReaderHandle,
    StoredValue,
};

pub enum PageError {
    Limit,
    Continuation,
    Order,
}
pub struct PageRules<E> {
    pub index: &'static str,
    pub error: fn(PageError, &'static str) -> E,
}
impl<E> PageRules<E> {
    fn fail(&self, kind: PageError) -> E {
        (self.error)(kind, self.index)
    }
    pub fn check_continuation(&self, page: &ScanPage) -> Result<(), E> {
        if page
            .next_after
            .as_ref()
            .is_some_and(|next| page.entries.last().map(|entry| &entry.key) != Some(next))
        {
            return Err(self.fail(PageError::Continuation));
        }
        Ok(())
    }
    pub fn validate(&self, page: &ScanPage, after: Option<&Key>, limit: usize) -> Result<(), E> {
        if page.entries.len() > limit {
            return Err(self.fail(PageError::Limit));
        }
        self.check_continuation(page)?;
        let mut previous = after;
        for entry in &page.entries {
            if previous.is_some_and(|previous| previous.as_bytes() >= entry.key.as_bytes()) {
                return Err(self.fail(PageError::Order));
            }
            previous = Some(&entry.key);
        }
        Ok(())
    }
    pub fn ascending_ids<I: Copy + Ord>(
        &self,
        entries: &[ScanEntry],
        after: Option<I>,
        mut parse: impl FnMut(&ScanEntry) -> Result<I, E>,
    ) -> Result<Vec<I>, E> {
        let mut previous = after;
        entries
            .iter()
            .map(|entry| {
                let id = parse(entry)?;
                if previous.is_some_and(|previous| id <= previous) {
                    return Err(self.fail(PageError::Order));
                }
                previous = Some(id);
                Ok(id)
            })
            .collect()
    }
    pub fn id_page<I: Copy + Ord>(
        &self,
        page: ScanPage,
        after: Option<I>,
        parse: impl FnMut(&ScanEntry) -> Result<I, E>,
    ) -> Result<(Vec<I>, Option<I>), E> {
        self.check_continuation(&page)?;
        let ids = self.ascending_ids(&page.entries, after, parse)?;
        let next = if page.next_after.is_some() {
            Some(
                *ids.last()
                    .ok_or_else(|| self.fail(PageError::Continuation))?,
            )
        } else {
            None
        };
        Ok((ids, next))
    }
}

pub type BodyPage<I, T> = (Vec<(I, T)>, Option<I>);

pub struct BodyPageCodec<I, T, E> {
    pub parse_key: fn(&[u8]) -> Result<I, E>,
    pub decode: fn(I, &[u8]) -> Result<T, E>,
}
/// The continuation is decoded before any body, retaining repository failure order.
pub fn scan_bodies<I: Copy, T, E: From<StorageError>>(
    storage: &StorageReaderHandle,
    namespace: Namespace,
    request: ScanRequest<'_>,
    codec: BodyPageCodec<I, T, E>,
    rules: PageRules<E>,
) -> Result<BodyPage<I, T>, E> {
    let page = storage.scan_prefix(namespace, request)?;
    rules.validate(&page, request.after(), request.limit())?;
    let next_after = page
        .next_after
        .as_ref()
        .map(|key| (codec.parse_key)(key.as_bytes()))
        .transpose()?;
    let entries = page
        .entries
        .into_iter()
        .map(|entry| {
            let id = (codec.parse_key)(entry.key.as_bytes())?;
            Ok((id, (codec.decode)(id, entry.value.as_bytes())?))
        })
        .collect::<Result<Vec<_>, E>>()?;
    Ok((entries, next_after))
}

pub fn read_records<I: Copy, T, E: From<StorageError>>(
    storage: &StorageReaderHandle,
    namespace: Namespace,
    ids: &[I],
    key: impl FnMut(I) -> Result<Key, E>,
    mut decode: impl FnMut(I, StoredValue) -> Result<T, E>,
) -> Result<Vec<Option<T>>, E> {
    let keys = ids
        .iter()
        .copied()
        .map(key)
        .collect::<Result<Vec<_>, _>>()?;
    storage
        .get_records(namespace, &keys)?
        .into_iter()
        .zip(ids.iter().copied())
        .map(|(record, id)| record.map(|record| decode(id, record)).transpose())
        .collect()
}

/// Adapts the checked scan to the owning repository's identity and error types.
#[macro_export]
macro_rules! repository_primary_scan {
    ($reader:ident, $error:ident, $id:ty, $request:ident, $body:ty, $page:ident, $repository:path) => {
        impl $reader {
            fn scan_stored_primary(
                &self,
                request: $request,
                name: &'static str,
                decode: fn($id, &[u8]) -> Result<$body, $error>,
            ) -> Result<$page, $error> {
                use $repository as repository;
                let limit = repository::validate_id_page_request(request)?;
                let after = request.after.map(repository::item_key).transpose()?;
                let codec = $crate::repository_pages::BodyPageCodec {
                    parse_key: repository::parse_primary_key,
                    decode,
                };
                let (entries, next_after) = $crate::repository_pages::scan_bodies(
                    &self.storage,
                    repository::namespace(name)?,
                    $crate::ScanRequest::new(&[], after.as_ref(), limit)?,
                    codec,
                    repository::page_rules(name),
                )?;
                Ok($page {
                    entries,
                    next_after,
                })
            }
        }
    };
}
/// Retains each domain's exact typed failures and diagnostic prefix.
#[macro_export]
macro_rules! repository_page_rules {
    ($error:ident, $index:expr, $label:literal) => {
        $crate::repository_pages::PageRules {
            index: $index,
            error: |kind, index| match kind {
                $crate::repository_pages::PageError::Limit => $crate::StorageError::Corruption(
                    format!("{} {index} page exceeds requested limit", $label),
                )
                .into(),
                $crate::repository_pages::PageError::Continuation => {
                    $error::InvalidPageContinuation { index }
                }
                $crate::repository_pages::PageError::Order => $error::NonAscendingIdPage { index },
            },
        }
    };
}
