//! Shared execution-test fixture. Scenario assertions stay in their callers.

use alloy_primitives::{B256, U256};
use outbe_nod::NodContract;
use outbe_primitives::storage::StorageHandle;

pub(super) fn qualify(
    storage: &StorageHandle<'_>,
    bucket_key: B256,
    floor: U256,
    iso: u16,
) -> outbe_primitives::error::Result<()> {
    let issued_at = NodContract::new(storage.clone())
        .callable_bucket_issued_at
        .read(&bucket_key)?;
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let mut index = oracle.pair_index_of(pair)?;
    if index == 0 {
        index = outbe_oracle::api::register_pair(storage.clone(), pair)?;
    }
    let day = outbe_primitives::time::first_full_day(issued_at);
    oracle.record_utc_day_vwap(day, index, floor + U256::ONE)?;
    if oracle.utc_day_vwap_last_finalized.read()? < day {
        oracle.utc_day_vwap_last_finalized.write(day)?;
    }
    Ok(())
}
