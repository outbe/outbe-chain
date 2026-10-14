mod bodies;
mod cleanup;
mod index;

use std::collections::BTreeSet;

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::{
    api::{EntityRef, ExecutionScope},
    collection_key, partition_collection_key,
    schema::{
        body_locator, Collection, CompressedEntitiesSchema, DeltaStatus, IndexRecord, PendingWord,
        STORAGE_SCHEMA_VERSION,
    },
    CeDomain, Commitment, PartitionRef, RetirementOutcome, WwdEntityId,
};

// Cleanup is prepaid on the first touch. The provider separately meters the
// transaction's immediate SLOAD/SSTORE work. These reserves cover the later
// no-refund end-block zeroing only.
const SSTORE_RESET_GAS: u64 = 5_000;
pub(crate) const MAX_STORED_BODY_BYTES_V1: usize = 192;
/// The identity is two plain words now (slot 10 and slot 13), not a
/// dynamic byte record, so cleanup zeroes exactly two slots.
const BODY_IDENTITY_SLOTS: u64 = 2;
const MAX_INDEX_RECORD_BYTES: usize = 55;

const fn dynamic_storage_slots(bytes: usize) -> u64 {
    1 + bytes.div_ceil(32) as u64
}

pub(crate) const FIRST_BODY_TOUCH_CLEANUP_GAS: u64 = SSTORE_RESET_GAS
    * (1 + dynamic_storage_slots(MAX_STORED_BODY_BYTES_V1) + BODY_IDENTITY_SLOTS + 1);
pub(crate) const FIRST_INDEX_TOUCH_CLEANUP_GAS: u64 =
    SSTORE_RESET_GAS * (1 + dynamic_storage_slots(MAX_INDEX_RECORD_BYTES) + 1);
pub(crate) const BODY_TOUCHED_LENGTH_CLEANUP_GAS: u64 = SSTORE_RESET_GAS;
pub(crate) const INDEX_TOUCHED_LENGTH_CLEANUP_GAS: u64 = SSTORE_RESET_GAS;
pub(crate) const FIRST_RETIREMENT_TOUCH_CLEANUP_GAS: u64 = SSTORE_RESET_GAS * 2;
pub(crate) const RETIREMENT_TOUCHED_LENGTH_CLEANUP_GAS: u64 = SSTORE_RESET_GAS;

pub(crate) struct State<'storage> {
    storage: StorageHandle<'storage>,
}

impl<'storage> State<'storage> {
    pub(crate) fn new(storage: StorageHandle<'storage>) -> Self {
        Self { storage }
    }

    fn schema(&self) -> CompressedEntitiesSchema<'storage> {
        CompressedEntitiesSchema::new(self.storage.clone())
    }

    pub(crate) fn ensure_schema(&self) -> Result<()> {
        let schema = self.schema();
        let actual = schema.storage_schema_version.read()?;
        match actual {
            0 => {
                if !schema.last_smt_root.read()?.is_zero()
                    || !schema.reserved_2.read()?.is_zero()
                    || !schema.reserved_3.read()?.is_zero()
                {
                    return Err(fatal(
                        "compressed-entity schema initialization found non-empty root/reserved slots",
                    ));
                }
                #[cfg(test)]
                {
                    let root = crate::sealed_root(B256::ZERO).map_err(fatal)?;
                    schema
                        .last_smt_root
                        .write(U256::from_be_slice(root.as_slice()))?;
                    schema.storage_schema_version.write(STORAGE_SCHEMA_VERSION)
                }
                #[cfg(not(test))]
                {
                    Err(fatal(
                        "compressed-entity genesis allocation is missing; lazy initialization is forbidden",
                    ))
                }
            }
            STORAGE_SCHEMA_VERSION => Ok(()),
            2 => {
                if !schema.touched.is_empty()?
                    || !schema.touched_index_deltas.is_empty()?
                    || !schema.retirement_touched.is_empty()?
                {
                    return Err(fatal(
                        "compressed-entity schema migration requires an empty overlay",
                    ));
                }
                schema.storage_schema_version.write(STORAGE_SCHEMA_VERSION)
            }
            _ => Err(fatal(format!(
                "unsupported compressed-entity storage schema {actual}"
            ))),
        }
    }

    fn validate_schema_for_read(&self) -> Result<()> {
        let actual = self.schema().storage_schema_version.read()?;
        match actual {
            STORAGE_SCHEMA_VERSION => Ok(()),
            #[cfg(test)]
            0 => Ok(()),
            #[cfg(not(test))]
            0 => Err(fatal(
                "compressed-entity genesis allocation is missing; reads cannot bootstrap it",
            )),
            _ => Err(fatal(format!(
                "unsupported compressed-entity storage schema {actual}"
            ))),
        }
    }

    pub(crate) fn root(&self) -> Result<B256> {
        self.validate_schema_for_read()?;
        let schema = self.schema();
        if !schema.reserved_2.read()?.is_zero() || !schema.reserved_3.read()?.is_zero() {
            return Err(fatal(
                "compressed-entity reserved schema slots are non-zero",
            ));
        }
        Ok(B256::from(schema.last_smt_root.read()?.to_be_bytes::<32>()))
    }

    pub(crate) fn write_root(&self, root: B256) -> Result<()> {
        self.ensure_schema()?;
        let schema = self.schema();
        if !schema.reserved_2.read()?.is_zero() || !schema.reserved_3.read()?.is_zero() {
            return Err(fatal(
                "compressed-entity reserved schema slots are non-zero",
            ));
        }
        schema.last_smt_root.write(U256::from_be_bytes(root.0))
    }

    pub(crate) fn request_retirement(
        &self,
        scope: &ExecutionScope,
        partition: PartitionRef,
    ) -> Result<RetirementOutcome> {
        self.ensure_schema()?;
        let (_, key) = partition_collection_key(partition).map_err(fatal)?;
        let storage_key = B256::from(*key.as_bytes());
        if !self
            .schema()
            .pending_retirement
            .read(&storage_key)?
            .is_zero()
        {
            return Err(fatal("duplicate compressed-entity retirement request"));
        }
        for (collection, entity_id, _) in self.final_body_mutations()? {
            let domain = match collection {
                Collection::Tribute => CeDomain::Tribute,
                Collection::NodItem => CeDomain::NodItem,
                Collection::NodBucket => CeDomain::NodBucket,
                Collection::Gem => CeDomain::Gem,
                Collection::GemBucket => CeDomain::GemBucket,
            };
            if collection_key(domain, entity_id).map_err(fatal)? == key {
                return Err(fatal(
                    "collection retirement conflicts with an earlier entity mutation",
                ));
            }
        }
        if !scope.partition_present_verified(partition)? {
            return Ok(RetirementOutcome::NotPresent);
        }
        let schema = self.schema();
        let cleanup_gas = FIRST_RETIREMENT_TOUCH_CLEANUP_GAS
            + if schema.retirement_touched.is_empty()? {
                RETIREMENT_TOUCHED_LENGTH_CLEANUP_GAS
            } else {
                0
            };
        scope.deduct_explicit_gas(&self.storage, cleanup_gas)?;
        schema
            .pending_retirement
            .write(&storage_key, U256::from(1))?;
        let PartitionRef::TributeWwd(day) = partition;
        schema.retirement_touched.push(day.value())?;
        Ok(RetirementOutcome::Requested)
    }

    pub(crate) fn retirements(&self) -> Result<Vec<PartitionRef>> {
        self.validate_schema_for_read()?;
        let schema = self.schema();
        let days = schema.retirement_touched.read_all()?;
        let mut unique = BTreeSet::new();
        let mut retirements = Vec::with_capacity(days.len());
        for day in days {
            let partition =
                PartitionRef::TributeWwd(outbe_primitives::time::WorldwideDay::new(day));
            let (_, key) = partition_collection_key(partition).map_err(fatal)?;
            let storage_key = B256::from(*key.as_bytes());
            if !unique.insert(storage_key)
                || self.schema().pending_retirement.read(&storage_key)? != U256::from(1)
            {
                return Err(fatal("malformed compressed-entity retirement journal"));
            }
            retirements.push(partition);
        }
        Ok(retirements)
    }

    pub(crate) fn assert_clean_begin(&self) -> Result<()> {
        self.ensure_schema()?;
        let schema = self.schema();
        if !schema.reserved_2.read()?.is_zero() || !schema.reserved_3.read()?.is_zero() {
            return Err(fatal(
                "compressed-entity reserved schema slots are non-zero",
            ));
        }
        if !schema.touched.is_empty()?
            || !schema.touched_index_deltas.is_empty()?
            || !schema.retirement_touched.is_empty()?
        {
            return Err(fatal(
                "compressed-entity block overlay is dirty at begin_block",
            ));
        }
        Ok(())
    }
}

fn fatal(message: impl core::fmt::Display) -> PrecompileError {
    crate::errors::fatal_error(message)
}
