//! Only a temporary e2e build responds to these hooks.
//! A scenario without an auction seeds the daily VWAPs and issues its series directly.
//! The forfeit sweep opens a day only after that day closes.
//! `closeCallNoticeForTest` moves a lapsed deadline into a closed day that the sweep opens in the next block.

use alloy_primitives::Bytes;
use alloy_sol_types::{sol, SolCall};
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexFactoryTestArming {
        function seedDayVwapsForTest(uint16 isoCode, uint32 days, uint256 value) external;
        function issueForTest(
            bytes14[] seriesIds,
            uint16[] issuanceCurrencies,
            uint32 worldwideDay,
            uint32 issuedAt,
            uint32 issuedUnits,
            uint128 promisLoadMinor,
            uint256 entryPriceMinor,
            uint16 referenceCurrency,
            uint32[] snapshotChains
        ) external;
        function closeCallNoticeForTest(bytes14 seriesId, uint64 deadline) external;
        function rescanClosedDayForTest() external;
    }
}

/// This function moves the called group of `series_id` so its transit close falls at `close_at`, in
/// that day. A notice that lapsed minutes ago would otherwise wait for midnight.
fn requeue_called(storage: &StorageHandle<'_>, series_id: SeriesId, close_at: u64) -> Result<()> {
    use outbe_intex::IntexContract;

    let mut intex = IntexContract::new(storage.clone());
    let group = outbe_intex::api::read_series(storage, series_id)?
        .group_key()
        .expect("date step");
    if intex.called_deadline.read(&group)? == 0 {
        return Err(crate::errors::IntexFactoryError::NoCalledGroupForTest.into());
    }
    // Members read the deadline from the call, so the call moves with the queue.
    let terms = u64::from(intex.call_group_notice_seconds.read(&group)?)
        + u64::from(intex.call_group_transit_seconds.read(&group)?);
    let called_at = u32::try_from(close_at.saturating_sub(terms).max(1))
        .map_err(|_| crate::errors::IntexFactoryError::BlockTimestampExceedsU32)?;
    intex.call_group_called_at.write(&group, called_at)?;
    intex.requeue_called(group, close_at)?;
    reopen_day(storage, close_at)
}

fn reopen_day(storage: &StorageHandle<'_>, deadline: u64) -> Result<()> {
    let intex = outbe_intex::IntexContract::new(storage.clone());
    let day = outbe_primitives::time::timestamp_to_date_key(deadline);
    intex.call_sweep_day.write(day)?;
    intex
        .call_latest_day
        .write(intex.call_latest_day.read()?.max(day))?;
    intex.daily_genesis_day.write(day)?;
    intex.daily_phase.write(0)?;
    for currency in outbe_oracle::api::reference_currencies(storage.clone())? {
        intex.call_bin_cursor.clear(&currency)?;
        intex.call_scan_failed_day.clear(&currency)?;
    }
    intex.call_currency_cursor.write(0)
}

/// Runs a test-arming call, or returns `None` for any other calldata.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    use IIntexFactoryTestArming::*;
    if rescanClosedDayForTestCall::abi_decode(data).is_ok() {
        return Some(guarded(storage, || {
            let yesterday = storage
                .now_secs()?
                .checked_sub(outbe_primitives::time::SECONDS_PER_DAY)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::revert(
                        "no closed UTC day for test rescan",
                    )
                })?;
            reopen_day(storage, yesterday)
        }));
    }
    if let Ok(call) = seedDayVwapsForTestCall::abi_decode(data) {
        return Some(guarded(storage, || seed_day_vwaps_for_test(storage, call)));
    }
    if let Ok(call) = issueForTestCall::abi_decode(data) {
        return Some(guarded(storage, || issue_for_test(storage, call)));
    }
    if let Ok(call) = closeCallNoticeForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_called(storage, SeriesId::from(call.seriesId), call.deadline)
        }));
    }
    None
}

fn guarded(storage: &StorageHandle<'_>, hook: impl FnOnce() -> Result<()>) -> Result<Bytes> {
    outbe_primitives::dispatch::ensure_mutation_allowed(storage)?;
    hook().map(|()| Bytes::new())
}

fn seed_day_vwaps_for_test(
    storage: &StorageHandle<'_>,
    call: IIntexFactoryTestArming::seedDayVwapsForTestCall,
) -> Result<()> {
    // What `set_vwap` does in this module's own tests: the per-day value keyed by
    // the pair's registry index, and the watermark the begin-block hook would move.
    // This adds nothing to the Oracle crate. It only writes data for the days the crate serves.
    use outbe_oracle::schema::OracleContract;
    use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

    let oracle = OracleContract::new(storage.clone());
    let pair = outbe_oracle::api::AddressPair::new_coen_to(call.isoCode);
    let pair_id = oracle.pair_index_of(pair)?;
    let mut day = previous_date_key(timestamp_to_date_key(storage.now_secs()?)).expect("date step");
    for _ in 0..call.days {
        oracle.record_utc_day_vwap(day, pair_id, call.value)?;
        if oracle.utc_day_vwap_last_finalized.read()? < day {
            oracle.utc_day_vwap_last_finalized.write(day)?;
        }
        day = previous_date_key(day).expect("date step");
    }
    Ok(())
}

fn issue_for_test(
    storage: &StorageHandle<'_>,
    call: IIntexFactoryTestArming::issueForTestCall,
) -> Result<()> {
    // Mirrors the clearing engine: issue every series first, then send the day's
    // legs once. Sending per series would declare a one-chunk day twice, and the
    // second delivery is dropped as a conflicting repeat rather than applied.
    if call.seriesIds.len() != call.issuanceCurrencies.len() {
        return Err(crate::errors::IntexFactoryError::IssueForTestCurrencyCountMismatch.into());
    }
    // The clearing engine fixes the call terms at the auction start. This hook has no
    // auction, so it fixes them from the profile that is live now.
    let terms = crate::api::call_terms(storage)?;
    let mut legs = Vec::new();
    let ids = call.seriesIds.clone();
    for (series_id, issuance_currency) in call.seriesIds.into_iter().zip(call.issuanceCurrencies) {
        legs.extend(crate::api::issue_intex(
            storage,
            crate::schema::IssuanceParams {
                series_id: SeriesId::from(series_id),
                worldwide_day: call.worldwideDay.into(),
                issued_units: call.issuedUnits,
                promis_load_minor: call.promisLoadMinor,
                entry_price_minor: call.entryPriceMinor,
                call_price_minor: terms.call_price(call.entryPriceMinor)?,
                call_trigger: terms.call_trigger,
                issuance_currency,
                reference_currency: call.referenceCurrency,
                snapshot_chains: call.snapshotChains.clone(),
            },
        )?);
    }
    // The Called sweep counts breach days from `issued_at`, so a scenario that
    // seeds those days places issuance behind them, on every chain alike. Zero
    // keeps the stamp the engine wrote.
    if call.issuedAt != 0 {
        for series_id in ids {
            outbe_intex::api::set_issued_at(storage, SeriesId::from(series_id), call.issuedAt)?;
        }
        for leg in &mut legs {
            leg.payload.issuedAt = call.issuedAt;
        }
    }
    crate::api::send_issuance(storage, legs)
}
