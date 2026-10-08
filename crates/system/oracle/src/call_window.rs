//! The trailing finalized daily VWAPs a call sweep decides against.

use alloy_primitives::U256;
use outbe_primitives::{
    call_breach::{breached_enough, BreachTerms, ScanTerms},
    error::Result,
    storage::StorageHandle,
    time::previous_date_key,
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
            day = previous_date_key(day);
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
}

/// The windows one slice reads, at most one per currency.
pub struct CallWindows {
    last_day: u32,
    cache: Vec<(u16, CallWindow)>,
}

impl CallWindows {
    pub fn new(last_day: u32) -> Self {
        Self {
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
        storage: &StorageHandle<'_>,
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
                    CallWindow::load(storage, reference_currency, self.last_day, terms()?)?;
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

    const DAY: u32 = 86_400;

    fn days(prices: &[Option<u64>]) -> Vec<(u32, Option<U256>)> {
        prices
            .iter()
            .enumerate()
            .map(|(back, price)| (100 - back as u32, price.map(U256::from)))
            .collect()
    }

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
}
