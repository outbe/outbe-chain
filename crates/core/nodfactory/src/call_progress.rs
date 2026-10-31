//! How close an Issued Nod is to its group's call.

use alloy_primitives::U256;
use outbe_nod::{EffectiveState, NodContract};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{day_pass::PassDays, error::Result, storage::StorageHandle};

use crate::called::breach_terms;
use crate::errors::NodFactoryError;
use crate::precompile::INodFactory::CallProgress;

pub(crate) fn call_progress(storage: &StorageHandle<'_>, nod_id: U256) -> Result<CallProgress> {
    let item = outbe_nod::api::get_item(storage, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    if outbe_nod::api::state_of(storage, &item)? != EffectiveState::Issued {
        return Err(NodFactoryError::NodNotIssued.into());
    }
    let nod = NodContract::new(storage.clone());
    let call_terms = nod.read_call_terms(item.group_key)?;
    let terms = breach_terms(&nod, item.group_key, &call_terms)?;
    let pass = PassDays {
        current: nod.call_sweep_day.read()?,
        latest: nod.call_latest_day.read()?,
    };
    let progress = CallWindow::progress(storage, call_terms.reference_currency, &terms, pass)?;
    let count = progress.count;
    Ok(CallProgress {
        callWindowDays: count.window_days,
        callThresholdDays: count.threshold_days,
        daysChecked: count.days_checked,
        daysAboveCallPrice: count.days_above_call_price,
        minDaysToCall: progress.min_days_to_call,
        referenceCurrency: call_terms.reference_currency,
        callPriceMinor: terms.call_price,
        lastDayPriceMinor: progress.last_day_price.unwrap_or_default(),
    })
}
