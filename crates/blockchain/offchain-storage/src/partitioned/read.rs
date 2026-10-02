//! Datasource-independent ordered merge, driven entirely by injected routing.

use super::PartitionedStorage;
use crate::{Key, Namespace, ScanPage, ScanRequest, StorageError, StorageReader, StoredValue};

impl StorageReader for PartitionedStorage {
    fn storage_scope(
        &self,
        namespace: &Namespace,
        key: &Key,
    ) -> Result<Option<super::StorageScope>, StorageError> {
        if let Some(scope) = namespace.scope() {
            return Ok(Some(scope.clone()));
        }
        Ok(self
            .routing
            .point(namespace, key, self.source.as_ref())?
            .map(|location| location.scope))
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        let _guard = self.gate.read();
        let locations = match namespace.scope() {
            Some(scope) => vec![
                Some(super::ReadLocation {
                    scope: scope.clone(),
                    require_present: false
                });
                keys.len()
            ],
            None => self
                .routing
                .points(&namespace, keys, self.source.as_ref())?,
        };
        if locations.len() != keys.len() {
            return Err(StorageError::Corruption(
                "partition routing batch cardinality mismatch".into(),
            ));
        }
        let mut groups =
            std::collections::BTreeMap::<super::StorageScope, Vec<(usize, bool, Key)>>::new();
        for (index, location) in locations.into_iter().enumerate() {
            if let Some(location) = location {
                groups.entry(location.scope).or_default().push((
                    index,
                    location.require_present,
                    keys[index].clone(),
                ));
            }
        }
        let mut result = vec![None; keys.len()];
        for (scope, members) in groups {
            let keys: Vec<_> = members.iter().map(|(_, _, key)| key.clone()).collect();
            let records = match self.source.open_reader(&scope)? {
                Some(reader) => reader.get_records(namespace.logical(), &keys)?,
                None => vec![None; keys.len()],
            };
            if records.len() != members.len() {
                return Err(StorageError::Corruption(
                    "partition datasource batch cardinality mismatch".into(),
                ));
            }
            for ((index, required, _), record) in members.into_iter().zip(records) {
                if required && record.is_none() {
                    return Err(StorageError::Corruption(
                        "entity location points to missing primary body".into(),
                    ));
                }
                result[index] = record;
            }
        }
        Ok(result)
    }
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        let _guard = self.gate.read();
        let location = match namespace.scope() {
            Some(scope) => Some(super::ReadLocation {
                scope: scope.clone(),
                require_present: false,
            }),
            None => self.routing.point(&namespace, key, self.source.as_ref())?,
        };
        let Some(location) = location else {
            return Ok(None);
        };
        let record = match self.source.open_reader(&location.scope)? {
            Some(reader) => reader.get_record(namespace.logical(), key)?,
            None => None,
        };
        if location.require_present && record.is_none() {
            return Err(StorageError::Corruption(
                "entity location points to missing primary body".into(),
            ));
        }
        Ok(record)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        request.validate()?;
        let _guard = self.gate.read();
        let scopes = match namespace.scope() {
            Some(scope) => vec![scope.clone()],
            None => self
                .routing
                .scan(&namespace, request, self.source.as_ref())?,
        };
        if let [scope] = scopes.as_slice() {
            return match self.source.open_reader(scope)? {
                Some(reader) => {
                    let page = reader.scan_prefix(namespace.logical(), request)?;
                    super::scan::validate_page(&page, request)?;
                    Ok(page)
                }
                None => Ok(ScanPage::default()),
            };
        }
        let readers = scopes
            .iter()
            .map(|scope| self.source.open_reader(scope))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        super::scan::merge(readers, namespace.logical(), request)
    }
}
