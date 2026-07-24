use alloy_primitives::B256;
use outbe_primitives::{
    error::Result,
    storage::{dsl::missing_record_err, StorageHandle},
};

use crate::schema::MetadosisContract;

use super::schema::poc_schema_limits;

/// Returns one complete canonical OCB1 job record through the existing
/// Metadosis public precompile.
pub fn get_offchain_job(storage: StorageHandle<'_>, intent_id: B256) -> Result<Vec<u8>> {
    let limits = poc_schema_limits();
    let record = MetadosisContract::new(storage)
        .ocomp_job_record(intent_id, &limits)?
        .ok_or_else(|| missing_record_err("OcompJobRecordV1"))?;
    record
        .encode_canonical(&limits)
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))
}
