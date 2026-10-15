//! Hooks only a throwaway e2e build answers. A scenario that runs no auction seeds the
//! day VWAPs and issues its series straight. The forfeit sweep
//! opens a bucket only once its hour has closed, so `closeCallNoticeForTest` moves a
//! lapsed deadline into an hour the sweep opens on the next block.

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
            address[] recipients,
            uint256[] units,
            uint32[] recipientChains,
            uint32[] snapshotChains
        ) external;
        function closeCallNoticeForTest(uint16 isoCode, uint32 worldwideDay, uint64 deadline) external;
    }
}

/// Move a called group onto `deadline`, and with it into that deadline's bucket. The
/// sweep opens a bucket only once its hour has closed, so a notice that lapsed minutes
/// ago would otherwise idle out the rest of the hour.
fn requeue_called(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    worldwide_day: u32,
    deadline: u64,
) -> Result<()> {
    use outbe_intex::IntexContract;
    use outbe_primitives::time::WorldwideDay;

    let mut intex = IntexContract::new(storage.clone());
    let worldwide_day = WorldwideDay::from(worldwide_day);
    let key = IntexContract::scoped(iso_code, worldwide_day.value());
    if intex.called_deadline.read(&key)? == 0 {
        return Err(crate::errors::IntexFactoryError::NoCalledGroupForTest.into());
    }
    intex.remove_called(iso_code, worldwide_day)?;
    intex.push_called(iso_code, worldwide_day, deadline)
}

/// Runs a test-arming call, or returns `None` for any other calldata.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    use IIntexFactoryTestArming::*;
    if let Ok(call) = seedDayVwapsForTestCall::abi_decode(data) {
        return Some(guarded(storage, || seed_day_vwaps_for_test(storage, call)));
    }
    if let Ok(call) = issueForTestCall::abi_decode(data) {
        return Some(guarded(storage, || issue_for_test(storage, call)));
    }
    if let Ok(call) = closeCallNoticeForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_called(storage, call.isoCode, call.worldwideDay, call.deadline)
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
    let mut day = previous_date_key(timestamp_to_date_key(storage.timestamp()?.to::<u64>()));
    for _ in 0..call.days {
        oracle.record_utc_day_vwap(day, pair_id, call.value)?;
        if oracle.utc_day_vwap_last_finalized.read()? < day {
            oracle.utc_day_vwap_last_finalized.write(day)?;
        }
        day = previous_date_key(day);
    }
    // The VWAP pusher sends the rewritten days again.
    let factory = crate::schema::IntexFactoryContract::new(storage.clone());
    if factory.vwap_sent_day.read()? > day {
        factory.vwap_sent_day.write(day)?;
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
                issuance_currency,
                reference_currency: call.referenceCurrency,
                recipients: call.recipients.clone(),
                units: call.units.clone(),
                recipient_chains: call.recipientChains.clone(),
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
