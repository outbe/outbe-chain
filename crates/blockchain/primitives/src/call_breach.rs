//! The rule that calls a right, and the span one currency's call sweep must read.
//!
//! A right is called when the COEN price in its reference currency sat strictly
//! above its call price on at least its threshold days of its trailing window.
//! Both terms are sealed onto the right, so retuning a profile cannot re-term it.

use alloy_primitives::{keccak256, B256, U256};

use crate::error::Result;
use crate::storage::dsl::Map;
use crate::time::SECONDS_PER_DAY_U32;

/// The call terms one right was sealed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreachTerms {
    pub call_price: U256,
    pub window_seconds: u32,
    pub threshold_seconds: u32,
    /// The right's first full UTC day. Earlier days never count: it did not exist yet.
    pub start_day: u32,
}

/// The call group of a right. Its members start counting on the same day against the
/// same call price in the same currency, so one window decides them all.
pub fn group_key(full_day_after_issue: u32, call_price: U256, reference_currency: u16) -> B256 {
    let mut bytes = [0u8; 38];
    bytes[..4].copy_from_slice(&full_day_after_issue.to_be_bytes());
    bytes[4..36].copy_from_slice(&call_price.to_be_bytes::<32>());
    bytes[36..].copy_from_slice(&reference_currency.to_be_bytes());
    keccak256(bytes)
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
    if threshold_seconds < SECONDS_PER_DAY_U32 {
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
        live_threshold_seconds >= SECONDS_PER_DAY_U32,
    ) {
        (0, _) => live_threshold_seconds,
        (stored, true) => stored.min(live_threshold_seconds),
        (stored, false) => stored,
    };
    Ok(ScanTerms {
        window_days: window / SECONDS_PER_DAY_U32,
        threshold_days: threshold / SECONDS_PER_DAY_U32,
    })
}

/// Where a right stands in its trailing window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreachCount {
    pub window_days: u32,
    pub threshold_days: u32,
    /// Window days from the start day on, priced or not.
    pub days_checked: u32,
    pub days_above_call_price: u32,
}

impl BreachCount {
    const fn calls(&self) -> bool {
        self.window_days != 0 && self.threshold_days != 0 && self.threshold_days <= self.window_days
    }

    const fn breached(&self) -> bool {
        self.calls() && self.days_above_call_price >= self.threshold_days
    }
}

/// Whether each window day from the start day on sat above the call price, newest first.
fn days_above<'a>(
    vwaps: &'a [(u32, Option<U256>)],
    terms: &BreachTerms,
    window_days: u32,
) -> impl Iterator<Item = bool> + 'a {
    let call_price = terms.call_price;
    let start_day = terms.start_day;
    vwaps
        .iter()
        .take(window_days as usize)
        .take_while(move |(day, _)| *day >= start_day)
        .map(move |(_, vwap)| vwap.is_some_and(|value| value > call_price))
}

/// Days at or below the call price and days with no published price both fail to
/// count, so the window absorbs up to `window - threshold` of either. Zero terms
/// mean no terms, not a breach on every day. The walk stops at the first day before
/// `start_day`; the window is newest first, so everything past it is older still.
pub fn count_breaches(vwaps: &[(u32, Option<U256>)], terms: &BreachTerms) -> BreachCount {
    let window_days = terms.window_seconds / SECONDS_PER_DAY_U32;
    let (mut days_checked, mut days_above_call_price) = (0, 0);
    for above in days_above(vwaps, terms, window_days) {
        days_checked += 1;
        days_above_call_price += u32::from(above);
    }
    BreachCount {
        window_days,
        threshold_days: terms.threshold_seconds / SECONDS_PER_DAY_U32,
        days_checked,
        days_above_call_price,
    }
}

pub fn breached_enough(vwaps: &[(u32, Option<U256>)], terms: &BreachTerms) -> bool {
    count_breaches(vwaps, terms).breached()
}

/// The fewest further days above the call price that call the right, while the oldest
/// days leave the window. `u32::MAX` for terms that never call.
pub fn min_days_to_call(vwaps: &[(u32, Option<U256>)], terms: &BreachTerms) -> u32 {
    let count = count_breaches(vwaps, terms);
    if !count.calls() {
        return u32::MAX;
    }
    (0..=count.threshold_days)
        .find(|&added| {
            let kept = count.days_checked.min(count.window_days - added);
            let still_above = days_above(vwaps, terms, kept)
                .filter(|&above| above)
                .count();
            added + still_above as u32 >= count.threshold_days
        })
        .unwrap_or(count.threshold_days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::call_breach_prices as days;

    const DAY: u32 = SECONDS_PER_DAY_U32;

    fn terms(price: u64, window: u32, threshold: u32, start_day: u32) -> BreachTerms {
        BreachTerms {
            call_price: U256::from(price),
            window_seconds: window * DAY,
            threshold_seconds: threshold * DAY,
            start_day,
        }
    }

    #[test]
    fn each_group_input_splits_the_group() {
        let base = group_key(20260310, U256::from(4_560_000), 840);
        assert_ne!(base, group_key(20260311, U256::from(4_560_000), 840));
        assert_ne!(base, group_key(20260310, U256::from(4_560_001), 840));
        assert_ne!(base, group_key(20260310, U256::from(4_560_000), 978));
    }

    #[test]
    fn a_midnight_issue_and_a_later_one_that_day_are_in_different_groups() {
        let midnight = crate::time::first_full_day(1_773_100_800).expect("date step");
        let later = crate::time::first_full_day(1_773_100_802).expect("date step");
        assert_ne!(
            group_key(midnight, U256::from(4_560_000), 840),
            group_key(later, U256::from(4_560_000), 840)
        );
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
    fn the_count_covers_the_whole_window_from_the_start_day() {
        let vwaps = days(&[Some(11), None, Some(10), Some(12), Some(13)]);
        let count = count_breaches(&vwaps, &terms(10, 4, 2, 97));
        assert_eq!((count.days_checked, count.days_above_call_price), (4, 2));
        assert_eq!(min_days_to_call(&vwaps, &terms(10, 4, 2, 97)), 0);
    }

    #[test]
    fn old_days_above_the_call_price_leave_the_window_first() {
        let mut oldest_above = vec![None; 13];
        oldest_above.extend([Some(11); 15]);
        let count = count_breaches(&days(&oldest_above), &terms(10, 28, 21, 0));
        assert_eq!((count.days_checked, count.days_above_call_price), (28, 15));
        assert_eq!(
            min_days_to_call(&days(&oldest_above), &terms(10, 28, 21, 0)),
            21
        );

        let mut newest_above = vec![Some(11); 15];
        newest_above.extend([None; 13]);
        assert_eq!(
            min_days_to_call(&days(&newest_above), &terms(10, 28, 21, 0)),
            6
        );
    }

    #[test]
    fn a_young_window_keeps_its_days() {
        let vwaps = days(&[Some(11); 15]);
        assert_eq!(
            count_breaches(&vwaps, &terms(10, 28, 21, 86)).days_checked,
            15
        );
        assert_eq!(min_days_to_call(&vwaps, &terms(10, 28, 21, 86)), 6);
    }

    #[test]
    fn zero_or_inverted_terms_never_breach() {
        let vwaps = days(&[Some(11); 3]);
        assert!(!breached_enough(&vwaps, &terms(10, 3, 0, 0)));
        assert!(!breached_enough(&vwaps, &terms(10, 0, 0, 0)));
        assert!(!breached_enough(&vwaps, &terms(10, 2, 3, 0)));
        assert_eq!(min_days_to_call(&vwaps, &terms(10, 2, 3, 0)), u32::MAX);
    }
}
