//! These hooks serve only E2E tests. A Credis receives its issuance timestamp when issued.
//! Only a temporary build can move that timestamp before the closed days that a scenario initializes for the call sweep.
//! The forfeit sweep opens a day only after that day closes.
//! Thus, `closeCallNoticeForTest` moves a lapsed deadline into a day that the sweep opens on the next block.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_primitives::dispatch::ensure_mutation_allowed;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_credis::CredisContract;

use crate::errors::CredisFactoryError;

sol! {
    interface ICredisFactoryTestArming {
        function backdateCredisForTest(uint256 credisId, uint64 issuedAt) external;
        function closeCallNoticeForTest(uint256 credisId, uint64 deadline) external;
        function rescanClosedDayForTest() external;
    }
}

/// Runs a test-arming call, or returns `None` for any other calldata.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    use ICredisFactoryTestArming::*;
    if let Ok(call) = backdateCredisForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            backdate(storage, call.credisId, call.issuedAt)
        }));
    }
    if let Ok(call) = closeCallNoticeForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_called(storage, call.credisId, call.deadline)
        }));
    }
    if rescanClosedDayForTestCall::abi_decode(data).is_ok() {
        return Some(guarded(storage, || {
            let day = outbe_system_common::time::previous_date_key(
                outbe_system_common::time::timestamp_to_date_key(storage.now_secs()?),
            )
            .map_err(date_step_error)?;
            reopen_day(storage, day)
        }));
    }
    None
}

fn guarded(storage: &StorageHandle<'_>, hook: impl FnOnce() -> Result<()>) -> Result<Bytes> {
    ensure_mutation_allowed(storage)?;
    hook().map(|()| Bytes::new())
}

fn date_step_error(
    err: outbe_system_common::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => {
            outbe_credis::errors::CredisError::DateStep { kind, date_key }.into()
        }
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

/// Moves issuance and the interest anchor together, so no interest is skipped. The
/// Credis moves to the series of its new issuance day.
fn backdate(storage: &StorageHandle<'_>, credis_id: U256, issued_at: u64) -> Result<()> {
    let mut credis = CredisContract::new(storage.clone());
    let mut record = credis.get_credis(credis_id)?;
    credis.leave_series(record.series_id, credis_id)?;
    credis.take_series_unpaid_gratis(record.series_id, record.outstanding_gratis_minor)?;
    credis.delete_credis_record(credis_id)?;
    record.issued_at = issued_at;
    record.last_settled_at = issued_at;
    record.series_id = CredisContract::series_of(
        issued_at,
        record.issuance_currency,
        record.reference_currency,
    )?;
    let call_start_day =
        outbe_system_common::time::first_full_day(issued_at).map_err(date_step_error)?;
    // `create_credis_record` books the original collateral. A backdated Credis keeps
    // only what is still outstanding in the series sum.
    let booked = record.gratis_minor;
    credis.create_credis_record(&record, call_start_day)?;
    let overbooked = booked
        .checked_sub(record.outstanding_gratis_minor)
        .ok_or(outbe_credis::errors::CredisError::ArithmeticOverflow)?;
    if !overbooked.is_zero() {
        credis.take_series_unpaid_gratis(record.series_id, overbooked)?;
    }
    Ok(())
}

/// Moves the deadline of the Credis's called series.
fn requeue_called(storage: &StorageHandle<'_>, credis_id: U256, deadline: u64) -> Result<()> {
    let mut credis = CredisContract::new(storage.clone());
    let series = credis.get_credis(credis_id)?.series_id;
    if credis.called_deadline.read(&series)? == 0 {
        return Err(CredisFactoryError::NotCalled.into());
    }
    // Members read the deadline from the call, so the call moves with the queue.
    let notice = u64::from(credis.series_call_notice_period_seconds.read(&series)?);
    credis
        .series_called_at
        .write(&series, deadline.saturating_sub(notice).max(1))?;
    credis.remove_called(series)?;
    credis.push_called(series, deadline)?;
    let day = outbe_system_common::time::timestamp_to_date_key(deadline);
    reopen_day(storage, day)
}

fn reopen_day(storage: &StorageHandle<'_>, day: u32) -> Result<()> {
    let credis = CredisContract::new(storage.clone());
    if day > credis.call_latest_day.read()? {
        credis.call_latest_day.write(day)?;
    }
    credis.call_sweep_day.write(day)?;
    credis.daily_phase.write(0)?;
    credis.call_currency_cursor.write(0)?;
    for currency in outbe_oracle::api::REFERENCE_CURRENCIES {
        credis.call_bin_cursor.write(&currency, 0)?;
        credis.call_scan_failed_day.clear(&currency)?;
    }
    Ok(())
}
