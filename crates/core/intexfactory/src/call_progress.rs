//! How close an Issued series is to its group's call.

use alloy_primitives::U256;
use outbe_intex::{IntexContract, IntexState};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{day_pass::PassDays, error::Result, storage::StorageHandle};

use crate::called::breach_terms;
use crate::errors::{date_step_error, IntexFactoryError};
use crate::precompile::IIntexFactory::CallProgress;

pub(crate) fn call_progress(storage: &StorageHandle<'_>, intex_id: U256) -> Result<CallProgress> {
    let record = outbe_intex::api::find_series_by_id(storage, intex_id)?
        .ok_or(outbe_intex::IntexError::SeriesNotFound)?;
    if outbe_intex::api::state(storage, &record)? != IntexState::Issued {
        return Err(IntexFactoryError::SeriesNotIssued.into());
    }
    let intex = IntexContract::new(storage.clone());
    let terms = breach_terms(
        &intex,
        &record,
        record.group_key().map_err(date_step_error)?,
    )?;
    let pass = PassDays {
        current: intex.call_sweep_day.read()?,
        latest: intex.call_latest_day.read()?,
    };
    let progress = CallWindow::progress(storage, record.reference_currency, &terms, pass)?;
    let count = progress.count;
    Ok(CallProgress {
        callWindowDays: count.window_days,
        callThresholdDays: count.threshold_days,
        daysChecked: count.days_checked,
        daysAboveCallPrice: count.days_above_call_price,
        minDaysToCall: progress.min_days_to_call,
        referenceCurrency: record.reference_currency,
        callPriceMinor: terms.call_price,
        lastDayPriceMinor: progress.last_day_price.unwrap_or_default(),
    })
}
