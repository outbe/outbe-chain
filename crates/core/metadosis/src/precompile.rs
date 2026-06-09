use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_common::WorldwideDay;
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::schema::MetadosisContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IMetadosis.sol"
);

/// Dispatches an ABI-encoded call to the Metadosis precompile (view-only).
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IMetadosis::IMetadosisCalls::abi_decode, |call| {
        let metadosis = MetadosisContract::new(storage);
        use IMetadosis::IMetadosisCalls::*;
        match call {
            getWorldwideDay(c) => view(c, |c| {
                let Some(day) = metadosis.worldwide_days.get(c.wwd.into())? else {
                    return Err(outbe_primitives::storage::dsl::missing_record_err(
                        "WorldwideDay",
                    ));
                };
                Ok((
                    day.status,
                    day.day_type,
                    day.forming_start,
                    day.forming_end,
                    day.lookback_end,
                    day.offering_end,
                    day.scheduled_process_time,
                    day.previous_vwap,
                    day.current_vwap,
                )
                    .into())
            }),
            getDayMetadosisLimit(c) => view(c, |c| {
                let date = WorldwideDay::from(c.date);
                let amount = metadosis.get_day_limit(date)?;
                let is_used = metadosis.is_day_limit_used(date)?;
                Ok((amount, is_used).into())
            }),
            getActiveWorldwideDays(_) => metadata::<IMetadosis::getActiveWorldwideDaysCall>(|| {
                let wwds = metadosis.get_all_active_wwds()?;
                Ok(wwds.into_iter().map(u32::from).collect())
            }),
            getWorldwideDaysByStatus(c) => view(c, |c| {
                let wwds = metadosis.get_active_wwds_by_status(c.status)?;
                Ok(wwds.into_iter().map(u32::from).collect())
            }),
            getBootstrapEndTime(_) => metadata::<IMetadosis::getBootstrapEndTimeCall>(|| {
                metadosis.get_bootstrap_end_time()
            }),
        }
    })
}
