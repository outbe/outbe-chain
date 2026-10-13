//! Membership delta transitions and first-touch prepayment.

use super::*;

impl State<'_> {
    pub(crate) fn apply_index_add(
        &self,
        scope: &ExecutionScope,
        record: &IndexRecord,
    ) -> Result<()> {
        self.apply_index_delta(scope, record, DeltaStatus::Added)
    }
    pub(crate) fn apply_index_remove(
        &self,
        scope: &ExecutionScope,
        record: &IndexRecord,
    ) -> Result<()> {
        self.apply_index_delta(scope, record, DeltaStatus::Removed)
    }
    fn apply_index_delta(
        &self,
        scope: &ExecutionScope,
        record: &IndexRecord,
        target: DeltaStatus,
    ) -> Result<()> {
        let current = self.validate_or_prepare_delta(scope, record)?;
        let next = match current {
            DeltaStatus::NeverTouched | DeltaStatus::NoChangeTouched => target,
            current if current == target => {
                let message = match target {
                    DeltaStatus::Added => "index add requested for an already-current membership",
                    _ => "index remove requested for a non-current membership",
                };
                return Err(fatal(message));
            }
            _ => DeltaStatus::NoChangeTouched,
        };
        self.schema()
            .index_delta_word
            .write(&record.key(), next.encode())
    }
    pub(super) fn validate_or_prepare_delta(
        &self,
        scope: &ExecutionScope,
        record: &IndexRecord,
    ) -> Result<DeltaStatus> {
        self.ensure_schema()?;
        let key = record.key();
        let schema = self.schema();
        let current = DeltaStatus::decode(schema.index_delta_word.read(&key)?)?;
        let stored = schema.index_delta_record.get_bytes(&key).read()?;
        if current == DeltaStatus::NeverTouched {
            if !stored.is_empty() {
                return Err(fatal("untouched index delta has residual record"));
            }
            let cleanup_gas = FIRST_INDEX_TOUCH_CLEANUP_GAS
                + if schema.touched_index_deltas.is_empty()? {
                    INDEX_TOUCHED_LENGTH_CLEANUP_GAS
                } else {
                    0
                };
            scope.deduct_explicit_gas(&self.storage, cleanup_gas)?;
            schema
                .index_delta_record
                .get_bytes(&key)
                .write(&record.encode())?;
            schema.touched_index_deltas.push(key)?;
        } else if stored != record.encode() || IndexRecord::decode(&stored)?.key() != key {
            return Err(fatal("compressed-entity index record/hash mismatch"));
        }
        Ok(current)
    }
    pub(crate) fn index_deltas(&self) -> Result<Vec<(IndexRecord, DeltaStatus)>> {
        self.validate_schema_for_read()?;
        let schema = self.schema();
        let keys = schema.touched_index_deltas.read_all()?;
        let mut unique = BTreeSet::new();
        let mut records = Vec::with_capacity(keys.len());
        for key in keys {
            if !unique.insert(key) {
                return Err(fatal("duplicate compressed-entity touched index key"));
            }
            let status = DeltaStatus::decode(schema.index_delta_word.read(&key)?)?;
            if status == DeltaStatus::NeverTouched {
                return Err(fatal("touched index key has zero status"));
            }
            let bytes = schema.index_delta_record.get_bytes(&key).read()?;
            let record = IndexRecord::decode(&bytes)?;
            if record.key() != key {
                return Err(fatal("compressed-entity touched index hash mismatch"));
            }
            records.push((record, status));
        }
        Ok(records)
    }
}
