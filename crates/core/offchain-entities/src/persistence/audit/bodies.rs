use super::{store::RecordSorter, CeAuditError, CeAuditVisitor, CeAuditWork};
use crate::{
    collection_key,
    persistence::{LeafValue, TreeKey, TreeNamespace},
    proof::canonical_body_leaf,
    schema::Collection,
    sharding::shard_index,
    smt::derive_tree_key,
    CeDomain, WwdEntityId,
};

/// Compares canonical primary bodies with the complete leaf stream from `audit_exact`.
/// Catalog and live leaf populations are separate; empty materialized collections
/// legitimately have a catalog entry without primary bodies.
pub struct CeBodyAudit<'a> {
    expected: RecordSorter<'a, 101>,
    actual: RecordSorter<'a, 101>,
    catalog: RecordSorter<'a, 64>,
}

#[derive(Debug)]
pub struct CeBodyAuditReport {
    pub bodies: u64,
    /// Largest individual in-memory run, independent of total data size.
    pub peak_buffered_records: usize,
}

impl<'a> CeBodyAudit<'a> {
    pub fn create(work: &'a CeAuditWork) -> Result<Self, CeAuditError> {
        Ok(Self {
            expected: RecordSorter::new(work, 69)?,
            actual: RecordSorter::new(work, 69)?,
            catalog: RecordSorter::new(work, 32)?,
        })
    }

    pub fn push_body(
        &mut self,
        domain: CeDomain,
        id: WwdEntityId,
        encoded: &[u8],
    ) -> Result<(), CeAuditError> {
        let leaf = canonical_body_leaf(domain, id, encoded).map_err(invalid)?;
        let collection = collection_key(domain, id).map_err(invalid)?;
        let kind = match domain {
            CeDomain::Tribute => Collection::Tribute,
            CeDomain::NodItem => Collection::NodItem,
            CeDomain::NodBucket => Collection::NodBucket,
        };
        let key = derive_tree_key(kind, id).map_err(invalid)?;
        let shard = shard_index(key, domain.shard_count()).map_err(invalid)?;
        let record = leaf_record(
            TreeNamespace::CollectionShard(collection, shard),
            TreeKey::try_from(alloy_primitives::B256::from(key.as_bytes()))?,
            LeafValue::try_from(leaf)?,
        );
        self.actual.push(record)
    }

    pub fn finish(self) -> Result<CeBodyAuditReport, CeAuditError> {
        let peak_buffered_records = self
            .expected
            .peak_buffered_records()
            .max(self.actual.peak_buffered_records())
            .max(self.catalog.peak_buffered_records());
        let mut expected = self.expected.finish()?;
        let mut actual = self.actual.finish()?;
        let mut catalog = self.catalog.finish()?;
        let mut current_collection = catalog.next().transpose()?;
        let mut bodies = 0_u64;
        loop {
            match (expected.next().transpose()?, actual.next().transpose()?) {
                (None, None) => break,
                (Some(a), Some(b)) if a == b => {
                    let collection = &a[1..33];
                    while current_collection
                        .as_ref()
                        .is_some_and(|row| &row[..32] < collection)
                    {
                        current_collection = catalog.next().transpose()?;
                    }
                    if current_collection
                        .as_ref()
                        .is_none_or(|row| &row[..32] != collection)
                    {
                        return Err(invalid("body collection is missing from catalog"));
                    }
                    bodies = bodies
                        .checked_add(1)
                        .ok_or_else(|| invalid("body count overflow"))?;
                }
                _ => return Err(invalid("live CE leaves and primary bodies differ")),
            }
        }
        for record in catalog {
            record?;
        }
        Ok(CeBodyAuditReport {
            bodies,
            peak_buffered_records,
        })
    }
}

impl CeAuditVisitor for CeBodyAudit<'_> {
    fn visit_leaf(
        &mut self,
        namespace: TreeNamespace,
        key: TreeKey,
        value: LeafValue,
    ) -> Result<(), CeAuditError> {
        match namespace {
            TreeNamespace::Catalog => {
                let mut record = [0; 64];
                record[..32].copy_from_slice(&key.encode());
                record[32..].copy_from_slice(&value.encode());
                self.catalog.push(record)
            }
            TreeNamespace::CollectionShard(_, _) => {
                self.expected.push(leaf_record(namespace, key, value))
            }
        }
    }
}

fn leaf_record(namespace: TreeNamespace, key: TreeKey, value: LeafValue) -> [u8; 101] {
    let mut record = [0; 101];
    record[..37].copy_from_slice(&namespace.encode());
    let mut key = key.encode();
    key.reverse(); // Match native CKB TreeKey::Ord within each namespace.
    record[37..69].copy_from_slice(&key);
    record[69..].copy_from_slice(&value.encode());
    record
}

fn invalid(error: impl std::fmt::Display) -> CeAuditError {
    CeAuditError::Invalid(error.to_string())
}
