//! How close an Issued Credis is to its group's call.

use alloy_primitives::U256;
use outbe_credis::{CredisContract, CredisState};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::day_pass::PassDays;

use crate::called::breach_terms;
use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory::CallProgress;

pub(crate) fn call_progress(storage: &StorageHandle<'_>, credis_id: U256) -> Result<CallProgress> {
    let credis = CredisContract::new(storage.clone());
    let record = credis.load_credis(credis_id)?;
    if outbe_credis::runtime::effective_state(&record, storage.now_secs()?)? != CredisState::Issued
    {
        return Err(CredisFactoryError::CredisNotIssued.into());
    }
    let terms = breach_terms(&credis, record.series_id)?;
    let pass = PassDays {
        current: credis.call_sweep_day.read()?,
        latest: credis.call_latest_day.read()?,
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
