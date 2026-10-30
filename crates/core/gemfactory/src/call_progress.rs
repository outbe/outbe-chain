//! How close an Issued gem is to its group's call.

use alloy_primitives::U256;
use outbe_gem::{GemContract, GemState};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{day_pass::PassDays, error::Result, storage::StorageHandle};

use crate::called::breach_terms;
use crate::errors::GemFactoryError;
use crate::precompile::IGemFactory::CallProgress;

pub(crate) fn call_progress(storage: &StorageHandle<'_>, gem_id: U256) -> Result<CallProgress> {
    let gem = outbe_gem::api::get_view(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if gem.effective_state(storage.now_secs()?) != GemState::Issued as u8 {
        return Err(GemFactoryError::GemNotIssued.into());
    }
    let terms = breach_terms(&gem.terms, gem.group.full_day_after_issue.value());
    let contract = GemContract::new(storage.clone());
    let pass = PassDays {
        current: contract.call_sweep_day.read()?,
        latest: contract.call_latest_day.read()?,
    };
    let progress = CallWindow::progress(storage, gem.terms.reference_currency, &terms, pass)?;
    let count = progress.count;
    Ok(CallProgress {
        callWindowDays: count.window_days,
        callThresholdDays: count.threshold_days,
        daysChecked: count.days_checked,
        daysAboveCallPrice: count.days_above_call_price,
        minDaysToCall: progress.min_days_to_call,
        referenceCurrency: gem.terms.reference_currency,
        callPriceMinor: terms.call_price,
        lastDayPriceMinor: progress.last_day_price.unwrap_or_default(),
    })
}
