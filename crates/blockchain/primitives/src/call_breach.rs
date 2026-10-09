//! The rule that calls a right, and the span one currency's call sweep must read.
//!
//! A right is called when the COEN price in its reference currency sat strictly
//! above its call price on at least its threshold days of its trailing window.
//! Both terms are sealed onto the right, so retuning a profile cannot re-term it.

use alloy_primitives::U256;

use crate::error::Result;
use crate::storage::dsl::Map;
use crate::time::SECONDS_PER_DAY;

const SECS_PER_DAY: u32 = SECONDS_PER_DAY as u32;

/// The call terms one right was sealed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreachTerms {
    pub call_price: U256,
    pub window_seconds: u32,
    pub threshold_seconds: u32,
    /// The right's first full UTC day. Earlier days never count: it did not exist yet.
    pub start_day: u32,
}

/// The span one currency's sweep reads: the widest window and the lowest threshold
/// any right priced in it may carry, in whole days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanTerms {
    pub window_days: u32,
    pub threshold_days: u32,
}

/// Widens a currency's stored scan terms to cover a newly sealed right.
pub fn widen_scan_terms(
    max_window: &Map<'_, u16, u32>,
    min_threshold: &Map<'_, u16, u32>,
    reference_currency: u16,
    window_seconds: u32,
    threshold_seconds: u32,
) -> Result<()> {
    if window_seconds > max_window.read(&reference_currency)? {
        max_window.write(&reference_currency, window_seconds)?;
    }
    // A threshold under a day can never be met, and would latch the range shut.
    if threshold_seconds < SECS_PER_DAY {
        return Ok(());
    }
    let min = min_threshold.read(&reference_currency)?;
    if min == 0 || threshold_seconds < min {
        min_threshold.write(&reference_currency, threshold_seconds)?;
    }
    Ok(())
}

/// The terms a currency's sweep must search to cover every live right: the stored
/// ones widened by the live profile, which the next right will be sealed with.
pub fn scan_terms(
    max_window: &Map<'_, u16, u32>,
    min_threshold: &Map<'_, u16, u32>,
    reference_currency: u16,
    live_window_seconds: u32,
    live_threshold_seconds: u32,
) -> Result<ScanTerms> {
    let window = max_window
        .read(&reference_currency)?
        .max(live_window_seconds);
    // A live threshold under a day is not one, as in `widen_scan_terms`.
    let threshold = match (
        min_threshold.read(&reference_currency)?,
        live_threshold_seconds >= SECS_PER_DAY,
    ) {
        (0, _) => live_threshold_seconds,
        (stored, true) => stored.min(live_threshold_seconds),
        (stored, false) => stored,
    };
    Ok(ScanTerms {
        window_days: window / SECS_PER_DAY,
        threshold_days: threshold / SECS_PER_DAY,
    })
}

/// Days at or below the call price and days with no published price both fail to
/// count, so the window absorbs up to `window - threshold` of either. Zero terms
/// mean no terms, not a breach on every day. The walk stops at the first day before
/// `start_day`; the window is newest first, so everything past it is older still.
pub fn breached_enough(vwaps: &[(u32, Option<U256>)], terms: &BreachTerms) -> bool {
    let window_days = terms.window_seconds / SECS_PER_DAY;
    let threshold_days = terms.threshold_seconds / SECS_PER_DAY;
    if window_days == 0 || threshold_days == 0 || threshold_days > window_days {
        return false;
    }
    let mut breaches: u32 = 0;
    for (day, vwap) in vwaps.iter().take(window_days as usize) {
        if *day < terms.start_day {
            break;
        }
        if vwap.is_some_and(|value| value > terms.call_price) {
            breaches += 1;
            if breaches >= threshold_days {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::call_breach_prices as days;

    const DAY: u32 = SECS_PER_DAY;

    fn terms(price: u64, window: u32, threshold: u32, start_day: u32) -> BreachTerms {
        BreachTerms {
            call_price: U256::from(price),
            window_seconds: window * DAY,
            threshold_seconds: threshold * DAY,
            start_day,
        }
    }

    #[test]
    fn a_breach_needs_threshold_days_strictly_above_the_call_price() {
        let vwaps = days(&[Some(11), Some(10), None, Some(12)]);
        assert!(breached_enough(&vwaps, &terms(10, 4, 2, 0)));
        assert!(!breached_enough(&vwaps, &terms(11, 4, 2, 0)));
        assert!(
            !breached_enough(&vwaps, &terms(10, 2, 2, 0)),
            "only its own window counts"
        );
    }

    #[test]
    fn days_before_the_start_day_never_count() {
        let vwaps = days(&[Some(11), Some(11), Some(11)]);
        assert!(breached_enough(&vwaps, &terms(10, 3, 2, 99)));
        assert!(!breached_enough(&vwaps, &terms(10, 3, 2, 100)));
    }

    #[test]
    fn a_live_threshold_under_a_day_leaves_the_stored_one() {
        use crate::storage::{dsl::Map, hashmap::HashMapStorageProvider, StorageHandle};
        let contract = alloy_primitives::address!("0x0000000000000000000000000000000000001003");
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let max_window: Map<'_, u16, u32> = Map::new(U256::from(0), contract, storage.clone());
            let min_threshold: Map<'_, u16, u32> = Map::new(U256::from(1), contract, storage);
            min_threshold.write(&840, 2 * DAY).unwrap();
            let terms = scan_terms(&max_window, &min_threshold, 840, 4 * DAY, DAY - 1).unwrap();
            assert_eq!((terms.window_days, terms.threshold_days), (4, 2));
            let terms = scan_terms(&max_window, &min_threshold, 840, 4 * DAY, DAY).unwrap();
            assert_eq!(terms.threshold_days, 1);
        });
    }

    #[test]
    fn zero_or_inverted_terms_never_breach() {
        let vwaps = days(&[Some(11); 3]);
        assert!(!breached_enough(&vwaps, &terms(10, 3, 0, 0)));
        assert!(!breached_enough(&vwaps, &terms(10, 0, 0, 0)));
        assert!(!breached_enough(&vwaps, &terms(10, 2, 3, 0)));
    }
}
