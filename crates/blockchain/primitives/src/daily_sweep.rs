//! Scheduling of a sweep that walks one closed UTC day at a time.

use crate::error::Result;
use crate::storage::dsl::Value;

/// The day a sweep is walking and the one waiting behind it, 0 for none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepDays {
    pub current: u32,
    pub pending: u32,
}

/// What scheduling a closed day did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduled {
    /// Nothing was running, so the day starts now.
    Opened,
    /// An earlier day is still being walked, so the day waits behind it.
    Queued,
    /// The day took the place of `skipped`, which will never be walked.
    Replaced { skipped: u32 },
    /// The day is already being walked or already waits.
    Ignored,
}

impl SweepDays {
    /// Schedules `day` without disturbing a sweep in flight. One day waits at most,
    /// so a backlog shows up as skipped days rather than as a growing queue.
    pub fn schedule(mut self, day: u32) -> (Self, Scheduled) {
        let scheduled = if day <= self.current.max(self.pending) {
            Scheduled::Ignored
        } else if self.current == 0 {
            self = Self {
                current: day,
                pending: 0,
            };
            Scheduled::Opened
        } else if self.pending == 0 {
            self.pending = day;
            Scheduled::Queued
        } else {
            let skipped = core::mem::replace(&mut self.pending, day);
            Scheduled::Replaced { skipped }
        };
        (self, scheduled)
    }

    /// The current day was walked to its end, so the waiting one starts.
    pub fn finish(self) -> Self {
        Self {
            current: self.pending,
            pending: 0,
        }
    }
}

/// The stored current and pending days of one sweep.
pub struct PinnedDay<'a, 'storage> {
    pub current: &'a Value<'storage, u32>,
    pub pending: &'a Value<'storage, u32>,
}

impl PinnedDay<'_, '_> {
    /// Schedules `day` and stores the day that waits. An opened day is stored by [`Self::open`].
    pub fn schedule(&self, day: u32) -> Result<(SweepDays, Scheduled)> {
        let days = SweepDays {
            current: self.current.read()?,
            pending: self.pending.read()?,
        };
        let (next, scheduled) = days.schedule(day);
        if matches!(scheduled, Scheduled::Queued | Scheduled::Replaced { .. }) {
            self.pending.write(next.pending)?;
        }
        Ok((next, scheduled))
    }

    pub fn open(&self, days: SweepDays) -> Result<()> {
        self.current.write(days.current)?;
        self.pending.write(days.pending)
    }

    /// Ends the walk of `pinned`. Returns the days to open next, or `None` once idle.
    pub fn finish(&self, pinned: u32) -> Result<Option<SweepDays>> {
        let next = SweepDays {
            current: pinned,
            pending: self.pending.read()?,
        }
        .finish();
        if next.current == 0 {
            self.current.write(0)?;
            return Ok(None);
        }
        Ok(Some(next))
    }
}

/// Index of the currency the cursor names, or the head when the registry dropped it.
pub fn currency_position(currencies: &[u16], cursor: u32) -> usize {
    u16::try_from(cursor)
        .ok()
        .and_then(|iso| currencies.iter().position(|&code| code == iso))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_AGO: u32 = 20251231;
    const YESTERDAY: u32 = 20260101;
    const TODAY: u32 = 20260102;

    fn days(current: u32, pending: u32) -> SweepDays {
        SweepDays { current, pending }
    }

    #[test]
    fn a_closed_day_opens_an_idle_sweep_and_waits_behind_a_running_one() {
        assert_eq!(
            SweepDays::default().schedule(TODAY),
            (days(TODAY, 0), Scheduled::Opened)
        );
        assert_eq!(
            days(YESTERDAY, 0).schedule(TODAY),
            (days(YESTERDAY, TODAY), Scheduled::Queued)
        );
    }

    #[test]
    fn a_day_already_walked_or_waiting_is_left_alone() {
        for scheduled in [days(TODAY, 0), days(YESTERDAY, TODAY)] {
            assert_eq!(scheduled.schedule(TODAY), (scheduled, Scheduled::Ignored));
        }
        assert_eq!(
            days(TODAY, 0).schedule(YESTERDAY),
            (days(TODAY, 0), Scheduled::Ignored)
        );
    }

    #[test]
    fn a_newer_day_replaces_the_one_waiting_and_names_it() {
        assert_eq!(
            days(TWO_AGO, YESTERDAY).schedule(TODAY),
            (
                days(TWO_AGO, TODAY),
                Scheduled::Replaced { skipped: YESTERDAY }
            )
        );
    }

    #[test]
    fn finishing_starts_the_waiting_day_or_goes_idle() {
        assert_eq!(days(YESTERDAY, TODAY).finish(), days(TODAY, 0));
        assert_eq!(days(TODAY, 0).finish(), SweepDays::default());
    }

    #[test]
    fn a_finished_day_can_be_walked_again() {
        let finished = days(TODAY, 0).finish();
        assert_eq!(finished.schedule(TODAY).1, Scheduled::Opened);
    }

    fn with_pinned(test: impl FnOnce(&PinnedDay<'_, '_>)) {
        use crate::storage::{hashmap::HashMapStorageProvider, StorageHandle};
        use alloy_primitives::{address, U256};
        let contract = address!("0x0000000000000000000000000000000000001003");
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let current = Value::new(U256::from(1), contract, storage.clone());
            let pending = Value::new(U256::from(2), contract, storage);
            test(&PinnedDay {
                current: &current,
                pending: &pending,
            });
        });
    }

    #[test]
    fn a_pinned_day_stores_the_waiting_day_and_leaves_opening_to_the_caller() {
        with_pinned(|pinned| {
            assert_eq!(
                pinned.schedule(YESTERDAY).unwrap(),
                (days(YESTERDAY, 0), Scheduled::Opened)
            );
            assert_eq!(pinned.current.read().unwrap(), 0);
            pinned.open(days(YESTERDAY, 0)).unwrap();
            assert_eq!(
                pinned.schedule(TODAY).unwrap(),
                (days(YESTERDAY, TODAY), Scheduled::Queued)
            );
            assert_eq!(
                (
                    pinned.current.read().unwrap(),
                    pinned.pending.read().unwrap()
                ),
                (YESTERDAY, TODAY)
            );
            assert_eq!(pinned.schedule(TODAY).unwrap().1, Scheduled::Ignored);
        });
    }

    #[test]
    fn finishing_a_pinned_day_hands_over_the_waiting_one_or_goes_idle() {
        with_pinned(|pinned| {
            pinned.open(days(YESTERDAY, TODAY)).unwrap();
            assert_eq!(pinned.finish(YESTERDAY).unwrap(), Some(days(TODAY, 0)));
            pinned.open(days(TODAY, 0)).unwrap();
            assert_eq!(pinned.finish(TODAY).unwrap(), None);
            assert_eq!(pinned.current.read().unwrap(), 0);
        });
    }

    #[test]
    fn a_cursor_names_its_currency_or_falls_back_to_the_head() {
        let currencies = [840, 978];
        assert_eq!(currency_position(&currencies, 978), 1);
        assert_eq!(currency_position(&currencies, 392), 0);
        assert_eq!(currency_position(&currencies, u32::MAX), 0);
    }
}
