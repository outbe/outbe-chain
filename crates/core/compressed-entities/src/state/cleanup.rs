//! Clear and verify end-block journals in prepaid write order.

use super::*;

impl State<'_> {
    pub(crate) fn cleanup(&self) -> Result<()> {
        self.validate_schema_for_read()?;
        let schema = self.schema();
        let body_keys = schema.touched.read_all()?;
        self.cleanup_bodies(&body_keys)?;
        let index_keys = schema.touched_index_deltas.read_all()?;
        self.cleanup_indexes(&index_keys)?;
        let retirement_days = schema.retirement_touched.read_all()?;
        let unique_retirements = self.cleanup_retirements(&retirement_days)?;
        if !body_keys.is_empty() {
            schema.touched.clear()?;
        }
        if !index_keys.is_empty() {
            schema.touched_index_deltas.clear()?;
        }
        if !retirement_days.is_empty() {
            schema.retirement_touched.clear()?;
        }

        self.verify_cleanup(body_keys, index_keys, unique_retirements)
    }
    fn cleanup_bodies(&self, body_keys: &[B256]) -> Result<()> {
        let schema = self.schema();
        let mut unique_bodies = BTreeSet::new();
        for locator in body_keys {
            if !unique_bodies.insert(*locator) {
                return Err(fatal("duplicate compressed-entity touched body locator"));
            }
            let (collection, entity_id) = self
                .read_body_identity(*locator)?
                .ok_or_else(|| fatal("compressed-entity cleanup body has no identity"))?;
            if body_locator(collection, entity_id)? != *locator {
                return Err(fatal("compressed-entity cleanup body locator mismatch"));
            }
            let pending = PendingWord::decode(schema.pending_word.read(locator)?)?;
            let body = schema.pending_body.get_bytes(locator).read()?;
            match pending {
                PendingWord::Set(_) if body.is_empty() => {
                    return Err(fatal("compressed-entity cleanup Set entry has no body"));
                }
                PendingWord::Deleted if !body.is_empty() => {
                    return Err(fatal(
                        "compressed-entity cleanup Deleted entry retains body",
                    ));
                }
                PendingWord::Untouched => {
                    return Err(fatal("compressed-entity cleanup reached untouched body"));
                }
                _ => {}
            }
            schema.pending_body.get_bytes(locator).clear()?;
            self.clear_body_identity(*locator)?;
            schema.pending_word.write(locator, U256::ZERO)?;
        }

        Ok(())
    }
    fn cleanup_indexes(&self, index_keys: &[B256]) -> Result<()> {
        let schema = self.schema();
        let mut unique_indexes = BTreeSet::new();
        for key in index_keys {
            if !unique_indexes.insert(*key) {
                return Err(fatal("duplicate compressed-entity touched index key"));
            }
            let status = DeltaStatus::decode(schema.index_delta_word.read(key)?)?;
            if status == DeltaStatus::NeverTouched {
                return Err(fatal("compressed-entity cleanup reached untouched index"));
            }
            let record = schema.index_delta_record.get_bytes(key).read()?;
            if IndexRecord::decode(&record)?.key() != *key {
                return Err(fatal("compressed-entity cleanup index hash mismatch"));
            }
            schema.index_delta_record.get_bytes(key).clear()?;
            schema.index_delta_word.write(key, U256::ZERO)?;
        }

        Ok(())
    }
    fn cleanup_retirements(&self, retirement_days: &[u32]) -> Result<BTreeSet<B256>> {
        let schema = self.schema();
        let mut unique_retirements = BTreeSet::new();
        for day in retirement_days {
            let partition =
                PartitionRef::TributeWwd(outbe_primitives::time::WorldwideDay::new(*day));
            let (_, key) =
                partition_collection_key(partition).map_err(crate::errors::collection_error)?;
            let storage_key = B256::from(*key.as_bytes());
            if !unique_retirements.insert(storage_key)
                || schema.pending_retirement.read(&storage_key)? != U256::from(1)
            {
                return Err(fatal("malformed retirement journal during cleanup"));
            }
            schema.pending_retirement.write(&storage_key, U256::ZERO)?;
        }

        Ok(unique_retirements)
    }
    fn verify_cleanup(
        &self,
        body_keys: Vec<B256>,
        index_keys: Vec<B256>,
        unique_retirements: BTreeSet<B256>,
    ) -> Result<()> {
        let schema = self.schema();
        if !schema.touched.is_empty()?
            || !schema.touched_index_deltas.is_empty()?
            || !schema.retirement_touched.is_empty()?
        {
            return Err(fatal(
                "compressed-entity cleanup left non-empty touched lists",
            ));
        }
        for locator in body_keys {
            if !schema.pending_word.read(&locator)?.is_zero()
                || !schema.pending_body.get_bytes(&locator).is_empty()?
                || self.has_body_identity(locator)?
            {
                return Err(fatal(
                    "compressed-entity body cleanup post-condition failed",
                ));
            }
        }
        for key in index_keys {
            if !schema.index_delta_word.read(&key)?.is_zero()
                || !schema.index_delta_record.get_bytes(&key).is_empty()?
            {
                return Err(fatal(
                    "compressed-entity index cleanup post-condition failed",
                ));
            }
        }
        for storage_key in unique_retirements {
            if !schema.pending_retirement.read(&storage_key)?.is_zero() {
                return Err(fatal(
                    "compressed-entity retirement cleanup post-condition failed",
                ));
            }
        }
        Ok(())
    }
}
