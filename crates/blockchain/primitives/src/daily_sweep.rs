//! Scheduling of a sweep that walks one closed UTC day at a time, every closed day in
//! order.

use crate::error::{PrecompileError, Result};
use crate::storage::dsl::Value;
use crate::time::next_date_key;

/// The day a sweep is walking, 0 for none, and the newest closed day scheduled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepDays {
    pub current: u32,
    pub latest: u32,
}

/// What scheduling a closed day did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduled {
    /// Nothing was running, so the first day not yet walked starts now.
    Opened,
    /// An earlier day is still being walked, so the day waits behind it.
    Queued,
    /// The day is already scheduled.
    Ignored,
}

/// Completion is explicit: zero economic changes can still leave queued work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Pending,
    Complete,
}

impl SweepDays {
    /// Schedules `day` without disturbing a sweep in flight. Every closed day after the
    /// newest one scheduled is walked, in order, so a sweep that fell behind catches up
    /// rather than skipping days. Completed days cannot reopen.
    pub fn schedule(mut self, day: u32) -> (Self, Scheduled) {
        let idle = self.current == 0;
        let scheduled = if day <= self.latest || (!idle && day <= self.current) {
            Scheduled::Ignored
        } else if idle {
            let first = match self.latest {
                0 => day,
                latest => next_date_key(latest),
            };
            self = Self {
                current: first,
                latest: day,
            };
            Scheduled::Opened
        } else {
            self.latest = day;
            Scheduled::Queued
        };
        (self, scheduled)
    }

    /// The current day was walked to its end, so the next one starts, or the sweep idles.
    pub fn finish(self) -> Self {
        if self.current == 0 {
            return self;
        }
        let current = match self.current < self.latest {
            true => next_date_key(self.current),
            false => 0,
        };
        Self {
            current,
            latest: self.latest,
        }
    }
}

/// The stored current and newest scheduled days of one sweep.
pub struct PinnedDay<'a, 'storage> {
    pub current: &'a Value<'storage, u32>,
    pub latest: &'a Value<'storage, u32>,
}

impl PinnedDay<'_, '_> {
    fn days(&self) -> Result<SweepDays> {
        Ok(SweepDays {
            current: self.current.read()?,
            latest: self.latest.read()?,
        })
    }

    /// Schedules `day` and stores the newest day scheduled. An opened day is stored by
    /// [`Self::open`].
    pub fn schedule(&self, day: u32) -> Result<(SweepDays, Scheduled)> {
        let (next, scheduled) = self.days()?.schedule(day);
        if scheduled == Scheduled::Queued {
            self.latest.write(next.latest)?;
        }
        Ok((next, scheduled))
    }

    /// Marks `day` walked by an idle sweep with nothing to walk.
    pub fn pass(&self, day: u32) -> Result<()> {
        if day > self.latest.read()? {
            self.latest.write(day)?;
        }
        Ok(())
    }

    pub fn open(&self, days: SweepDays) -> Result<()> {
        self.current.write(days.current)?;
        self.latest.write(days.latest)
    }

    /// Ends the walk of `pinned`. Returns the days to open next, or `None` once idle.
    pub fn finish(&self, pinned: u32) -> Result<Option<SweepDays>> {
        if pinned == 0 || self.current.read()? != pinned || self.latest.read()? < pinned {
            return Err(PrecompileError::revert("invalid daily sweep completion"));
        }
        let next = SweepDays {
            current: pinned,
            latest: self.latest.read()?,
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

    fn days(current: u32, latest: u32) -> SweepDays {
        SweepDays { current, latest }
    }

    #[test]
    fn a_closed_day_opens_an_idle_sweep_and_waits_behind_a_running_one() {
        assert_eq!(
            SweepDays::default().schedule(TODAY),
            (days(TODAY, TODAY), Scheduled::Opened)
        );
        assert_eq!(
            days(YESTERDAY, YESTERDAY).schedule(TODAY),
            (days(YESTERDAY, TODAY), Scheduled::Queued)
        );
    }

    #[test]
    fn a_day_already_scheduled_is_left_alone() {
        for scheduled in [days(TODAY, TODAY), days(YESTERDAY, TODAY)] {
            assert_eq!(scheduled.schedule(TODAY), (scheduled, Scheduled::Ignored));
        }
        for scheduled in [days(TODAY, TODAY), days(0, TODAY)] {
            assert_eq!(
                scheduled.schedule(YESTERDAY),
                (scheduled, Scheduled::Ignored)
            );
        }
    }

    #[test]
    fn a_finished_day_cannot_be_walked_again() {
        let finished = days(TODAY, TODAY).finish();
        assert_eq!(finished.schedule(TODAY), (finished, Scheduled::Ignored));
    }

    #[test]
    fn every_day_a_stalled_sweep_missed_is_walked_in_order() {
        let (running, _) = days(TWO_AGO, YESTERDAY).schedule(TODAY);
        assert_eq!(running, days(TWO_AGO, TODAY));
        assert_eq!(running.finish(), days(YESTERDAY, TODAY));
        assert_eq!(running.finish().finish(), days(TODAY, TODAY));
        assert_eq!(running.finish().finish().finish(), days(0, TODAY));

        // An idle sweep picks up after the newest day it scheduled.
        assert_eq!(
            days(0, TWO_AGO).schedule(TODAY),
            (days(YESTERDAY, TODAY), Scheduled::Opened)
        );
    }

    #[test]
    fn finishing_starts_the_next_day_or_goes_idle() {
        assert_eq!(days(YESTERDAY, TODAY).finish(), days(TODAY, TODAY));
        assert_eq!(days(TODAY, TODAY).finish(), days(0, TODAY));
        assert_eq!(days(0, TODAY).finish(), days(0, TODAY));
    }

    fn with_pinned(test: impl FnOnce(&PinnedDay<'_, '_>)) {
        use crate::storage::{hashmap::HashMapStorageProvider, StorageHandle};
        use alloy_primitives::{address, U256};
        let contract = address!("0x0000000000000000000000000000000000001003");
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let current = Value::new(U256::from(1), contract, storage.clone());
            let latest = Value::new(U256::from(2), contract, storage);
            test(&PinnedDay {
                current: &current,
                latest: &latest,
            });
        });
    }

    #[test]
    fn a_pinned_day_stores_the_newest_day_and_leaves_opening_to_the_caller() {
        with_pinned(|pinned| {
            assert_eq!(
                pinned.schedule(YESTERDAY).unwrap(),
                (days(YESTERDAY, YESTERDAY), Scheduled::Opened)
            );
            assert_eq!(pinned.current.read().unwrap(), 0);
            pinned.open(days(YESTERDAY, YESTERDAY)).unwrap();
            assert_eq!(
                pinned.schedule(TODAY).unwrap(),
                (days(YESTERDAY, TODAY), Scheduled::Queued)
            );
            assert_eq!(
                (
                    pinned.current.read().unwrap(),
                    pinned.latest.read().unwrap()
                ),
                (YESTERDAY, TODAY)
            );
            assert_eq!(pinned.schedule(TODAY).unwrap().1, Scheduled::Ignored);
        });
    }

    #[test]
    fn finishing_a_pinned_day_hands_over_the_next_one_or_goes_idle() {
        with_pinned(|pinned| {
            pinned.open(days(YESTERDAY, TODAY)).unwrap();
            assert_eq!(pinned.finish(YESTERDAY).unwrap(), Some(days(TODAY, TODAY)));
            pinned.open(days(TODAY, TODAY)).unwrap();
            assert_eq!(pinned.finish(TODAY).unwrap(), None);
            assert_eq!(pinned.current.read().unwrap(), 0);
            assert_eq!(pinned.latest.read().unwrap(), TODAY);
        });
    }

    #[test]
    fn an_idle_pass_moves_the_newest_day_forward_only() {
        with_pinned(|pinned| {
            pinned.pass(TODAY).unwrap();
            pinned.pass(YESTERDAY).unwrap();
            assert_eq!(pinned.latest.read().unwrap(), TODAY);
            assert_eq!(pinned.current.read().unwrap(), 0);
        });
    }

    #[test]
    fn completion_rejects_a_stale_or_missing_pinned_day() {
        with_pinned(|pinned| {
            assert!(pinned.finish(0).is_err());
            pinned.open(days(YESTERDAY, TODAY)).unwrap();
            assert!(pinned.finish(TODAY).is_err());
            assert_eq!(pinned.current.read().unwrap(), YESTERDAY);
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
