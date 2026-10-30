//! The trailing finalized daily VWAPs a call sweep decides against.

use alloy_primitives::U256;
use outbe_primitives::{
    call_breach::{
        breached_enough, count_breaches, min_days_to_call, BreachCount, BreachTerms, ScanTerms,
    },
    day_pass::PassDays,
    error::Result,
    storage::StorageHandle,
    time::{previous_date_key, TimeError, SECONDS_PER_DAY_U32},
};

use crate::schema::OracleContract;

/// One currency's trailing finalized VWAPs, newest first, ending at `last_day`.
/// `None` marks a day the pair published no price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallWindow {
    vwaps: Vec<(u32, Option<U256>)>,
    ceiling: Option<U256>,
}

impl CallWindow {
    /// An unregistered pair yields an empty window: nothing priced in it can breach.
    pub fn load(
        storage: &StorageHandle<'_>,
        reference_currency: u16,
        last_day: u32,
        terms: ScanTerms,
    ) -> Result<Self> {
        let Some(pair_index) =
            crate::api::coen_pair_index_opt(storage.clone(), reference_currency)?
        else {
            return Ok(Self::from_vwaps(Vec::new(), terms.threshold_days));
        };
        let oracle = OracleContract::new(storage.clone());
        let mut vwaps = Vec::with_capacity(terms.window_days as usize);
        let mut day = last_day;
        for _ in 0..terms.window_days {
            vwaps.push((day, oracle.get_utc_day_vwap_for_pair(day, pair_index)?));
            match previous_date_key(day) {
                Ok(prev) => day = prev,
                Err(TimeError::DateBeforeMinimum { .. }) => break,
                Err(err) => return Err(crate::errors::date_step_error(err)),
            }
        }
        Ok(Self::from_vwaps(vwaps, terms.threshold_days))
    }

    pub fn from_vwaps(vwaps: Vec<(u32, Option<U256>)>, threshold_days: u32) -> Self {
        let mut priced: Vec<U256> = vwaps.iter().filter_map(|(_, vwap)| *vwap).collect();
        priced.sort_unstable_by(|a, b| b.cmp(a));
        let ceiling = threshold_days
            .checked_sub(1)
            .and_then(|index| priced.get(index as usize).copied());
        Self { vwaps, ceiling }
    }

    /// The highest call price any right can have breached: the threshold-th largest
    /// VWAP, at the currency's lowest threshold. `None` when too few days carry a price.
    pub fn ceiling(&self) -> Option<U256> {
        self.ceiling
    }

    pub fn breached(&self, terms: &BreachTerms) -> bool {
        breached_enough(&self.vwaps, terms)
    }

    /// One group's window as of the last finalized day. The pass walks every closed day
    /// in order, so a day it has not walked yet that already breaches decides the call.
    pub fn progress(
        storage: &StorageHandle<'_>,
        reference_currency: u16,
        terms: &BreachTerms,
        pass: PassDays,
    ) -> Result<CallProgress> {
        let last_day = OracleContract::new(storage.clone())
            .utc_day_vwap_last_finalized
            .read()?;
        if last_day == 0 {
            return Ok(CallProgress {
                count: count_breaches(&[], terms),
                min_days_to_call: min_days_to_call(&[], terms),
                last_day_price: None,
            });
        }
        let window_days = terms.window_seconds / SECONDS_PER_DAY_U32;
        let last_walked = pass.last_walked().map_err(crate::errors::date_step_error)?;
        let unwalked = days_between(last_walked, last_day, window_days)?;
        let window = Self::load(
            storage,
            reference_currency,
            last_day,
            ScanTerms {
                window_days: window_days + unwalked,
                threshold_days: 0,
            },
        )?;
        let decided = (1..=unwalked as usize).any(|back| {
            window
                .vwaps
                .get(back..)
                .is_some_and(|older| breached_enough(older, terms))
        });
        Ok(CallProgress {
            count: count_breaches(&window.vwaps, terms),
            min_days_to_call: if decided {
                0
            } else {
                min_days_to_call(&window.vwaps, terms)
            },
            last_day_price: window.vwaps.first().and_then(|(_, vwap)| *vwap),
        })
    }
}

/// One group's window as a view reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallProgress {
    pub count: BreachCount,
    pub min_days_to_call: u32,
    /// The VWAP of the last finalized day, `None` when the pair published none.
    pub last_day_price: Option<U256>,
}

/// The days strictly between `after` and `before`, at most `cap`.
fn days_between(after: u32, before: u32, cap: u32) -> Result<u32> {
    let mut days = 0;
    let mut day = before;
    while days < cap {
        day = match previous_date_key(day) {
            Ok(prev) => prev,
            Err(TimeError::DateBeforeMinimum { .. }) => break,
            Err(err) => return Err(crate::errors::date_step_error(err)),
        };
        if day <= after {
            break;
        }
        days += 1;
    }
    Ok(days)
}

/// The windows one slice reads, at most one per currency.
pub struct CallWindows<'storage> {
    storage: StorageHandle<'storage>,
    last_day: u32,
    cache: Vec<(u16, CallWindow)>,
}

impl<'storage> CallWindows<'storage> {
    pub fn new(storage: StorageHandle<'storage>, last_day: u32) -> Self {
        Self {
            storage,
            last_day,
            cache: Vec::new(),
        }
    }

    /// The closed day the windows end at.
    pub const fn last_day(&self) -> u32 {
        self.last_day
    }

    pub fn window(
        &mut self,
        reference_currency: u16,
        terms: impl FnOnce() -> Result<ScanTerms>,
    ) -> Result<&CallWindow> {
        let index = match self
            .cache
            .iter()
            .position(|(code, _)| *code == reference_currency)
        {
            Some(index) => index,
            None => {
                let window =
                    CallWindow::load(&self.storage, reference_currency, self.last_day, terms()?)?;
                self.cache.push((reference_currency, window));
                self.cache.len() - 1
            }
        };
        Ok(&self.cache[index].1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::test_utils::call_breach_prices as days;
    use outbe_primitives::time::SECONDS_PER_DAY_U32 as DAY;

    fn terms(price: u64, threshold: u32) -> BreachTerms {
        BreachTerms {
            call_price: U256::from(price),
            window_seconds: 4 * DAY,
            threshold_seconds: threshold * DAY,
            start_day: 0,
        }
    }

    #[test]
    fn the_ceiling_is_the_threshold_th_largest_price() {
        let window = CallWindow::from_vwaps(days(&[Some(5), None, Some(9), Some(7)]), 2);
        assert_eq!(window.ceiling(), Some(U256::from(7)));
        assert!(!window.breached(&terms(7, 2)));
        assert!(window.breached(&terms(6, 2)));
        assert_eq!(
            CallWindow::from_vwaps(days(&[Some(5), None]), 2).ceiling(),
            None
        );
        assert_eq!(CallWindow::from_vwaps(days(&[Some(5)]), 0).ceiling(), None);
    }

    #[test]
    fn a_day_the_pass_has_not_walked_decides_the_call() {
        use crate::api::{register_pair, AddressPair};
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;

        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let pair = register_pair(storage.clone(), AddressPair::new_coen_to(840)).unwrap();
            let oracle = OracleContract::new(storage.clone());
            for (day, vwap) in [(20260101, 5), (20260102, 11), (20260103, 11), (20260104, 5)] {
                oracle
                    .record_utc_day_vwap(day, pair, U256::from(vwap))
                    .unwrap();
            }
            oracle.utc_day_vwap_last_finalized.write(20260104).unwrap();
            let terms = BreachTerms {
                call_price: U256::from(10),
                window_seconds: 2 * DAY,
                threshold_seconds: 2 * DAY,
                start_day: 0,
            };
            let walked = |day| PassDays {
                current: 0,
                latest: day,
            };

            let pending = CallWindow::progress(&storage, 840, &terms, walked(20260102)).unwrap();
            assert_eq!(pending.min_days_to_call, 0);

            let walked = CallWindow::progress(&storage, 840, &terms, walked(20260103)).unwrap();
            assert_eq!(walked.count.days_checked, 2);
            assert_eq!(walked.count.days_above_call_price, 1);
            assert_eq!(walked.min_days_to_call, 2);
            assert_eq!(walked.last_day_price, Some(U256::from(5)));
        });
    }
}
