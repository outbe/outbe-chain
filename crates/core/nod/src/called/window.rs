use alloy_primitives::U256;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{error::Result, storage::StorageHandle, time::previous_date_key};

use super::VwapWindow;
use crate::{
    constants::{CALL_WINDOW, SECS_PER_DAY},
    schema::{CallTerms, NodContract},
};

/// The trailing finalized-VWAP windows one call walk reads, at most one per currency.
pub(super) struct VwapWindows<'a, 's> {
    oracle: &'a OracleContract<'s>,
    last_closed_day: u32,
    cache: Vec<(u16, VwapWindow)>,
}

impl<'a, 's> VwapWindows<'a, 's> {
    pub(super) fn new(oracle: &'a OracleContract<'s>, last_closed_day: u32) -> Self {
        Self {
            oracle,
            last_closed_day,
            cache: Vec::new(),
        }
    }

    /// The trailing finalized-VWAP window for `COEN/<iso>`, newest first,
    /// filling it on first use.
    ///
    /// An unregistered pair caches an empty window: a bucket in that currency never
    /// registers a breach.
    pub(super) fn window(
        &mut self,
        nod: &NodContract<'_>,
        storage: &StorageHandle<'_>,
        iso_code: u16,
    ) -> Result<&[(u32, Option<U256>)]> {
        let index = match self.cache.iter().position(|(code, _)| *code == iso_code) {
            Some(index) => index,
            None => {
                let window = self.load(nod, storage, iso_code)?;
                self.cache.push((iso_code, window));
                self.cache.len() - 1
            }
        };
        Ok(&self.cache[index].1)
    }

    fn load(
        &self,
        nod: &NodContract<'_>,
        storage: &StorageHandle<'_>,
        iso_code: u16,
    ) -> Result<VwapWindow> {
        let Some(pair_index) = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso_code)?
        else {
            return Ok(Vec::new());
        };
        // Widest of the current constant and anything ever armed. A bucket keeps
        // the window it was armed with, so a narrowed constant must not shorten
        // the span the scan collects for it.
        let window_days = nod
            .max_call_window_seconds
            .read(&iso_code)?
            .max(CALL_WINDOW)
            / SECS_PER_DAY;
        let mut window = Vec::with_capacity(window_days as usize);
        let mut day = self.last_closed_day;
        for _ in 0..window_days {
            window.push((day, self.oracle.get_utc_day_vwap_for_pair(day, pair_index)?));
            day = previous_date_key(day);
        }
        Ok(window)
    }
}

/// True when the bucket's trailing `call_window_seconds` carries at least its
/// `call_threshold_seconds` of days strictly above its `call_price_minor`.
///
/// Every term comes off the bucket, not from the constants, so a retune cannot
/// re-term a bucket that is already armed. `window` is sized for the widest
/// window in the currency, so this takes only its own prefix.
///
/// Days at or below the call price, and days with no published price, both
/// simply fail to count. The window therefore absorbs up to `window - threshold`
/// of either. The walk stops at the first UTC day preceding `first_full_day` of
/// the bucket's sealed `issued_at`. That `issued_at` is the logical issuance
/// time, however late the right materializes. Days before issuance and the
/// partial issuance UTC day therefore do not count. The window is newest-first,
/// so everything beyond that point is older still.
pub(super) fn breached_enough(
    window: &[(u32, Option<U256>)],
    terms: &CallTerms,
    start_day: u32,
) -> bool {
    let window_days = terms.call_window_seconds / SECS_PER_DAY;
    let threshold_days = terms.call_threshold_seconds / SECS_PER_DAY;
    if threshold_days > window_days {
        return false;
    }
    let mut breaches: u32 = 0;
    for (day, vwap) in window.iter().take(window_days as usize) {
        if *day < start_day {
            break;
        }
        if vwap.is_some_and(|value| value > terms.call_price_minor) {
            breaches += 1;
            if breaches >= threshold_days {
                return true;
            }
        }
    }
    false
}
